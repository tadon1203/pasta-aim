using System.Text;
using NiceAimassist.Capture;
using NiceAimassist.Display;
using NiceAimassist.Pipeline;
using Silk.NET.Core.Native;
using Silk.NET.Direct3D.Compilers;
using Silk.NET.Direct3D12;
using Silk.NET.DXGI;

namespace NiceAimassist.Gpu;

public sealed unsafe class Preprocessor : IDisposable
{
    // The destination is a typed buffer. The view format (R32_FLOAT or R16_FLOAT) sets the stored precision.
    private const string Shader = """
        Texture2D<float4> source_image : register(t0);
        RWBuffer<float> destination : register(u0);
        cbuffer Params : register(b0) { uint width; uint height; uint pixels; };
        [numthreads(8, 8, 1)]
        void main(uint3 id : SV_DispatchThreadID) {
            if (id.x >= width || id.y >= height) return;
            uint pixel = id.y * width + id.x;
            float4 color = source_image.Load(int3(id.x, id.y, 0));
            destination[pixel] = color.r;
            destination[pixels + pixel] = color.g;
            destination[2 * pixels + pixel] = color.b;
        }
        """;

    // D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING. This is the identity RGBA swizzle.
    private const uint DefaultComponentMapping = 0x1688;
    private const int ThreadsPerGroup = 8;

    private static readonly D3D12 D3D12Api = D3D12.GetApi();
    private static readonly D3DCompiler Compiler = D3DCompiler.GetApi();

    private readonly DesktopCapture _frames;
    private readonly DisplayDevices _gpu;
    private readonly ID3D12Device* _device;
    private readonly ID3D12CommandQueue* _queue;
    private readonly ID3D12Resource* _source;
    private readonly GpuDescriptorHandle _srvGpu;
    private readonly GpuDescriptorHandle _uavGpu;
    private readonly CpuDescriptorHandle _uavCpu;
    private readonly Format _format;
    private readonly uint _elementBytes;
    private readonly uint _elementCount;
    private readonly uint _dispatchX;
    private readonly uint _dispatchY;
    private readonly uint[] _constants;
    private ID3D12RootSignature* _rootSignature;
    private ID3D12PipelineState* _pipeline;
    private ID3D12DescriptorHeap* _heap;
    private ID3D12CommandAllocator* _allocator;
    private ID3D12GraphicsCommandList* _list;
    private ID3D12Resource* _destination;
    private bool _disposed;

    internal Preprocessor(DisplayDevices gpu, DesktopCapture frames, TensorPrecision precision)
    {
        _gpu = gpu;
        _frames = frames;
        _device = gpu.Device;
        _queue = gpu.ComputeQueue;
        _source = frames.SharedResource;

        int width = frames.InputWidth;
        int height = frames.InputHeight;
        _format = precision == TensorPrecision.Fp16 ? Format.FormatR16Float : Format.FormatR32Float;
        _elementBytes = (uint)ModelContract.ElementBytes(precision);
        _elementCount = (uint)(width * height * 3);
        _dispatchX = (uint)((width + ThreadsPerGroup - 1) / ThreadsPerGroup);
        _dispatchY = (uint)((height + ThreadsPerGroup - 1) / ThreadsPerGroup);
        _constants = [(uint)width, (uint)height, (uint)(width * height)];

        try
        {
            _rootSignature = CreateRootSignature();
            _pipeline = CreatePipeline();

            DescriptorHeapDesc heapDesc = new()
            {
                Type = DescriptorHeapType.CbvSrvUav,
                NumDescriptors = 2,
                Flags = DescriptorHeapFlags.ShaderVisible,
                NodeMask = 0,
            };
            ID3D12DescriptorHeap* heap;
            Com.Check(_device->CreateDescriptorHeap(&heapDesc, Com.IidOf<ID3D12DescriptorHeap>(), (void**)&heap), "CreateDescriptorHeap");
            _heap = heap;

            uint stride = _device->GetDescriptorHandleIncrementSize(DescriptorHeapType.CbvSrvUav);
            CpuDescriptorHandle cpuStart = _heap->GetCPUDescriptorHandleForHeapStart();
            GpuDescriptorHandle gpuStart = _heap->GetGPUDescriptorHandleForHeapStart();
            _srvGpu = gpuStart;
            _uavGpu = new GpuDescriptorHandle { Ptr = gpuStart.Ptr + stride };
            _uavCpu = new CpuDescriptorHandle { Ptr = cpuStart.Ptr + stride };

            ShaderResourceViewDesc srv = new()
            {
                Format = Format.FormatB8G8R8A8Unorm,
                ViewDimension = SrvDimension.Texture2D,
                Shader4ComponentMapping = DefaultComponentMapping,
            };
            srv.Anonymous.Texture2D = new Tex2DSrv { MostDetailedMip = 0, MipLevels = 1, PlaneSlice = 0, ResourceMinLODClamp = 0 };
            _device->CreateShaderResourceView(_source, &srv, cpuStart);

            ID3D12CommandAllocator* allocator;
            Com.Check(
                _device->CreateCommandAllocator(CommandListType.Compute, Com.IidOf<ID3D12CommandAllocator>(), (void**)&allocator),
                "CreateCommandAllocator");
            _allocator = allocator;

            ID3D12GraphicsCommandList* list;
            Com.Check(
                _device->CreateCommandList(
                    0, CommandListType.Compute, (ID3D12CommandAllocator*)_allocator, (ID3D12PipelineState*)_pipeline,
                    Com.IidOf<ID3D12GraphicsCommandList>(), (void**)&list),
                "CreateCommandList");
            _list = list;
            Com.Check(_list->Close(), "Close the preprocess command list");
        }
        catch
        {
            ReleaseAll();
            throw;
        }
    }

    // The guard rejects a stale frame. Only the newest captured frame can be submitted, and only once.
    internal void Submit(CapturedFrame frame, ID3D12Resource* destination)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        ulong sequence = frame.Sequence;
        if (sequence != _frames.Sequence || sequence <= _frames.SubmittedSequence)
        {
            throw new InvalidOperationException(
                $"The frame {sequence} cannot be submitted: the newest captured frame is {_frames.Sequence} and the last submitted is {_frames.SubmittedSequence}.");
        }

        if (_destination == null || destination != _destination)
        {
            Bind(destination);
        }

        Com.Check(_queue->Wait(_frames.CopyDoneFence, sequence), "Wait for the captured frame");
        ID3D12CommandList* list = (ID3D12CommandList*)_list;
        _queue->ExecuteCommandLists(1, &list);
        Com.Check(_queue->Signal(_frames.ComputeDoneFence, sequence), "Signal the compute done fence");
        _frames.MarkSubmitted(sequence);
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        // The queue can still read the source and the descriptor heap. Dispose this object before its DesktopCapture.
        NativeGpu.WaitForFence(_frames.ComputeDoneFence, _frames.SubmittedSequence);
        ReleaseAll();
    }

    private void ReleaseAll()
    {
        Com.Release(ref _list);
        Com.Release(ref _allocator);
        Com.Release(ref _heap);
        Com.Release(ref _pipeline);
        Com.Release(ref _rootSignature);
        _destination = null;
    }

    private void Bind(ID3D12Resource* destination)
    {
        ulong required = (ulong)_elementCount * _elementBytes;
        if (destination->GetDesc().Width < required)
        {
            throw new ArgumentException(
                $"The destination holds {destination->GetDesc().Width} bytes; the tensor needs {required}.",
                nameof(destination));
        }

        // The recorded list, the allocator, and the descriptor heap are reused. The GPU must finish them first.
        NativeGpu.WaitForFence(_frames.ComputeDoneFence, _frames.SubmittedSequence);
        Com.Check(_allocator->Reset(), "Reset the preprocess allocator");

        UnorderedAccessViewDesc uav = new() { Format = _format, ViewDimension = UavDimension.Buffer };
        uav.Anonymous.Buffer = new BufferUav
        {
            FirstElement = 0,
            NumElements = _elementCount,
            StructureByteStride = 0,
            CounterOffsetInBytes = 0,
        };
        _device->CreateUnorderedAccessView(destination, null, &uav, _uavCpu);

        Com.Check(_list->Reset(_allocator, _pipeline), "Reset the preprocess command list");
        ResourceBarrier toRead = D3D12Helpers.Transition(_source, ResourceStates.Common, ResourceStates.NonPixelShaderResource);
        _list->ResourceBarrier(1, &toRead);
        _list->SetComputeRootSignature(_rootSignature);
        ID3D12DescriptorHeap* heap = _heap;
        _list->SetDescriptorHeaps(1, &heap);
        _list->SetComputeRootDescriptorTable(0, _srvGpu);
        _list->SetComputeRootDescriptorTable(1, _uavGpu);
        fixed (uint* constants = _constants)
        {
            _list->SetComputeRoot32BitConstants(2, (uint)_constants.Length, constants, 0);
        }

        _list->Dispatch(_dispatchX, _dispatchY, 1);
        ResourceBarrier written = new() { Type = ResourceBarrierType.Uav, Flags = ResourceBarrierFlags.None };
        written.Anonymous.UAV = new ResourceUavBarrier { PResource = destination };
        _list->ResourceBarrier(1, &written);
        ResourceBarrier toCommon = D3D12Helpers.Transition(_source, ResourceStates.NonPixelShaderResource, ResourceStates.Common);
        _list->ResourceBarrier(1, &toCommon);
        Com.Check(_list->Close(), "Close the preprocess command list");

        _destination = destination;
    }

    private ID3D12RootSignature* CreateRootSignature()
    {
        DescriptorRange* ranges = stackalloc DescriptorRange[2];
        ranges[0] = new DescriptorRange
        {
            RangeType = DescriptorRangeType.Srv,
            NumDescriptors = 1,
            BaseShaderRegister = 0,
            RegisterSpace = 0,
            OffsetInDescriptorsFromTableStart = 0,
        };
        ranges[1] = new DescriptorRange
        {
            RangeType = DescriptorRangeType.Uav,
            NumDescriptors = 1,
            BaseShaderRegister = 0,
            RegisterSpace = 0,
            OffsetInDescriptorsFromTableStart = 0,
        };

        RootParameter* parameters = stackalloc RootParameter[3];
        parameters[0] = new RootParameter { ParameterType = RootParameterType.TypeDescriptorTable, ShaderVisibility = ShaderVisibility.All };
        parameters[0].Anonymous.DescriptorTable = new RootDescriptorTable { NumDescriptorRanges = 1, PDescriptorRanges = &ranges[0] };
        parameters[1] = new RootParameter { ParameterType = RootParameterType.TypeDescriptorTable, ShaderVisibility = ShaderVisibility.All };
        parameters[1].Anonymous.DescriptorTable = new RootDescriptorTable { NumDescriptorRanges = 1, PDescriptorRanges = &ranges[1] };
        parameters[2] = new RootParameter { ParameterType = RootParameterType.Type32BitConstants, ShaderVisibility = ShaderVisibility.All };
        parameters[2].Anonymous.Constants = new RootConstants { ShaderRegister = 0, RegisterSpace = 0, Num32BitValues = 3 };

        RootSignatureDesc description = new()
        {
            NumParameters = 3,
            PParameters = parameters,
            NumStaticSamplers = 0,
            PStaticSamplers = null,
            Flags = RootSignatureFlags.None,
        };
        ID3D10Blob* serialized;
        ID3D10Blob* errors;
        int result = D3D12Api.SerializeRootSignature(&description, D3DRootSignatureVersion.Version1, &serialized, &errors);
        if (result < 0)
        {
            string message = D3D12Helpers.BlobToString(errors);
            Com.Release(ref errors);
            throw new InvalidOperationException($"SerializeRootSignature failed (0x{result:X8}): {message}");
        }

        try
        {
            ID3D12RootSignature* signature;
            Com.Check(
                _device->CreateRootSignature(
                    0, serialized->GetBufferPointer(), serialized->GetBufferSize(), Com.IidOf<ID3D12RootSignature>(), (void**)&signature),
                "CreateRootSignature");
            return signature;
        }
        finally
        {
            Com.Release(ref serialized);
        }
    }

    private ID3D12PipelineState* CreatePipeline()
    {
        byte[] source = Encoding.UTF8.GetBytes(Shader);
        ID3D10Blob* bytecode;
        ID3D10Blob* errors;
        int result;
        fixed (byte* sourcePointer = source)
        {
            result = Compiler.Compile(
                sourcePointer, (nuint)source.Length, "preprocess.hlsl", null, null, "main", "cs_5_1", 0, 0, &bytecode, &errors);
        }

        if (result < 0)
        {
            string message = D3D12Helpers.BlobToString(errors);
            Com.Release(ref errors);
            throw new InvalidOperationException($"Compiling the preprocess shader failed (0x{result:X8}): {message}");
        }

        try
        {
            ComputePipelineStateDesc description = new()
            {
                PRootSignature = _rootSignature,
                CS = new ShaderBytecode { PShaderBytecode = bytecode->GetBufferPointer(), BytecodeLength = bytecode->GetBufferSize() },
            };
            ID3D12PipelineState* state;
            Com.Check(
                _device->CreateComputePipelineState(&description, Com.IidOf<ID3D12PipelineState>(), (void**)&state),
                "CreateComputePipelineState");
            return state;
        }
        finally
        {
            Com.Release(ref bytecode);
        }
    }
}
