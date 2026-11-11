using System.Runtime.InteropServices;

namespace NiceAimassist.Detect;

internal readonly record struct TensorIo(string Name, long[] Dims, int ElementType);

// The slot numbers are positions in the OrtApi and OrtDmlApi tables. Members are only appended.
// The numbers stay valid for every later version.
internal static unsafe partial class OrtApi
{
    private const uint ApiVersion = 17;

    private const int LoggingLevelWarning = 2;
    private const int ExecutionModeSequential = 0;
    private const int AllocatorTypeDevice = 0;
    private const int MemTypeDefault = 0;

    private static readonly nint Api = LoadApi();
    private static nint _dmlApi;

    private enum ApiSlot
    {
        GetErrorMessage = 2,
        CreateEnv = 3,
        CreateSession = 7,
        CreateSessionOptions = 10,
        SetSessionExecutionMode = 13,
        DisableMemPattern = 17,
        SetIntraOpNumThreads = 24,
        SessionGetInputCount = 30,
        SessionGetOutputCount = 31,
        SessionGetInputTypeInfo = 33,
        SessionGetOutputTypeInfo = 34,
        SessionGetInputName = 36,
        SessionGetOutputName = 37,
        CreateRunOptions = 39,
        CreateTensorAsOrtValue = 48,
        GetTensorMutableData = 51,
        CastTypeInfoToTensorInfo = 55,
        GetTensorElementType = 60,
        GetDimensionsCount = 61,
        GetDimensions = 62,
        CreateMemoryInfo = 68,
        AllocatorFree = 76,
        GetAllocatorWithDefaultOptions = 78,
        ReleaseEnv = 92,
        ReleaseStatus = 93,
        ReleaseMemoryInfo = 94,
        ReleaseSession = 95,
        ReleaseValue = 96,
        ReleaseRunOptions = 97,
        ReleaseTypeInfo = 98,
        ReleaseSessionOptions = 100,
        SessionGetModelMetadata = 111,
        ModelMetadataLookupCustomMetadataMap = 116,
        ReleaseModelMetadata = 118,
        AddSessionConfigEntry = 130,
        CreateAllocator = 131,
        ReleaseAllocator = 132,
        RunWithBinding = 133,
        CreateIoBinding = 134,
        ReleaseIoBinding = 135,
        BindInput = 136,
        BindOutput = 137,
        AddRunConfigEntry = 165,
        GetExecutionProviderApi = 195,
    }

    private enum DmlSlot
    {
        AppendExecutionProviderDml1 = 1,
        GetD3D12ResourceFromAllocation = 4,
    }

    [LibraryImport("onnxruntime.dll", EntryPoint = "OrtGetApiBase")]
    private static partial nint OrtGetApiBase();

    internal static nint CreateEnv(string logId)
    {
        nint id = Utf8(logId);
        try
        {
            nint env;
            Check(((delegate* unmanaged<int, nint, nint*, nint>)Slot(ApiSlot.CreateEnv))(
                LoggingLevelWarning, id, &env), "CreateEnv");
            return env;
        }
        finally
        {
            Marshal.FreeCoTaskMem(id);
        }
    }

    internal static void ReleaseEnv(nint env) =>
        ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseEnv))(env);

    internal static nint CreateSessionOptions()
    {
        nint options;
        Check(((delegate* unmanaged<nint*, nint>)Slot(ApiSlot.CreateSessionOptions))(&options),
            "CreateSessionOptions");
        return options;
    }

    internal static void ReleaseSessionOptions(nint options) =>
        ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseSessionOptions))(options);

    internal static void SetSequentialExecution(nint options) =>
        Check(((delegate* unmanaged<nint, int, nint>)Slot(ApiSlot.SetSessionExecutionMode))(
            options, ExecutionModeSequential), "SetSessionExecutionMode");

    internal static void DisableMemoryPattern(nint options) =>
        Check(((delegate* unmanaged<nint, nint>)Slot(ApiSlot.DisableMemPattern))(options),
            "DisableMemPattern");

    internal static void SetIntraOpThreads(nint options, int threads) =>
        Check(((delegate* unmanaged<nint, int, nint>)Slot(ApiSlot.SetIntraOpNumThreads))(
            options, threads), "SetIntraOpNumThreads");

    internal static void AddSessionConfig(nint options, string key, string value)
    {
        nint keyPtr = Utf8(key);
        nint valuePtr = Utf8(value);
        try
        {
            Check(((delegate* unmanaged<nint, nint, nint, nint>)Slot(ApiSlot.AddSessionConfigEntry))(
                options, keyPtr, valuePtr), "AddSessionConfigEntry");
        }
        finally
        {
            Marshal.FreeCoTaskMem(keyPtr);
            Marshal.FreeCoTaskMem(valuePtr);
        }
    }

    internal static nint CreateRunOptions()
    {
        nint options;
        Check(((delegate* unmanaged<nint*, nint>)Slot(ApiSlot.CreateRunOptions))(&options),
            "CreateRunOptions");
        return options;
    }

    internal static void ReleaseRunOptions(nint options) =>
        ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseRunOptions))(options);

    internal static void AddRunConfig(nint options, string key, string value)
    {
        nint keyPtr = Utf8(key);
        nint valuePtr = Utf8(value);
        try
        {
            Check(((delegate* unmanaged<nint, nint, nint, nint>)Slot(ApiSlot.AddRunConfigEntry))(
                options, keyPtr, valuePtr), "AddRunConfigEntry");
        }
        finally
        {
            Marshal.FreeCoTaskMem(keyPtr);
            Marshal.FreeCoTaskMem(valuePtr);
        }
    }

    // The device and the queue must belong to the same D3D12 device. They must outlive the session.
    internal static void AppendDirectMl1(nint options, nint dmlDevice, nint commandQueue) =>
        Check(((delegate* unmanaged<nint, nint, nint, nint>)DmlFunction(DmlSlot.AppendExecutionProviderDml1))(
            options, dmlDevice, commandQueue), "SessionOptionsAppendExecutionProvider_DML1");

    internal static nint CreateSession(nint env, string modelPath, nint options)
    {
        // ORTCHAR_T is wchar_t on Windows.
        nint path = Marshal.StringToHGlobalUni(modelPath);
        try
        {
            nint session;
            Check(((delegate* unmanaged<nint, nint, nint, nint*, nint>)Slot(ApiSlot.CreateSession))(
                env, path, options, &session), "CreateSession");
            return session;
        }
        finally
        {
            Marshal.FreeHGlobal(path);
        }
    }

    internal static void ReleaseSession(nint session) =>
        ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseSession))(session);

    internal static int GetIoCount(nint session, bool input)
    {
        nuint count;
        ApiSlot slot = input ? ApiSlot.SessionGetInputCount : ApiSlot.SessionGetOutputCount;
        Check(((delegate* unmanaged<nint, nuint*, nint>)Slot(slot))(session, &count), slot.ToString());
        return checked((int)count);
    }

    internal static TensorIo GetTensorIo(nint session, bool input, int index)
    {
        nint typeInfo = GetTypeInfo(session, input, index);
        try
        {
            nint tensorInfo;
            Check(((delegate* unmanaged<nint, nint*, nint>)Slot(ApiSlot.CastTypeInfoToTensorInfo))(
                typeInfo, &tensorInfo), "CastTypeInfoToTensorInfo");

            int elementType;
            Check(((delegate* unmanaged<nint, int*, nint>)Slot(ApiSlot.GetTensorElementType))(
                tensorInfo, &elementType), "GetTensorElementType");

            nuint rank;
            Check(((delegate* unmanaged<nint, nuint*, nint>)Slot(ApiSlot.GetDimensionsCount))(
                tensorInfo, &rank), "GetDimensionsCount");

            long[] dims = new long[checked((int)rank)];
            fixed (long* dimsPtr = dims)
            {
                Check(((delegate* unmanaged<nint, long*, nuint, nint>)Slot(ApiSlot.GetDimensions))(
                    tensorInfo, dimsPtr, rank), "GetDimensions");
            }

            return new TensorIo(GetIoName(session, input, index), dims, elementType);
        }
        finally
        {
            ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseTypeInfo))(typeInfo);
        }
    }

    internal static string? GetCustomMetadata(nint session, string key)
    {
        nint metadata;
        Check(((delegate* unmanaged<nint, nint*, nint>)Slot(ApiSlot.SessionGetModelMetadata))(
            session, &metadata), "SessionGetModelMetadata");
        try
        {
            nint allocator = DefaultAllocator();
            nint keyPtr = Utf8(key);
            nint value;
            try
            {
                Check(((delegate* unmanaged<nint, nint, nint, nint*, nint>)Slot(
                    ApiSlot.ModelMetadataLookupCustomMetadataMap))(metadata, allocator, keyPtr, &value),
                    "ModelMetadataLookupCustomMetadataMap");
            }
            finally
            {
                Marshal.FreeCoTaskMem(keyPtr);
            }

            if (value == 0)
            {
                return null;
            }

            try
            {
                return Marshal.PtrToStringUTF8(value);
            }
            finally
            {
                FreeWithAllocator(allocator, value);
            }
        }
        finally
        {
            ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseModelMetadata))(metadata);
        }
    }

    // The allocator is created from the session. Release it before the session.
    internal static nint CreateDirectMlAllocator(nint session)
    {
        nint name = Utf8("DML");
        nint memoryInfo;
        try
        {
            Check(((delegate* unmanaged<nint, int, int, int, nint*, nint>)Slot(ApiSlot.CreateMemoryInfo))(
                name, AllocatorTypeDevice, 0, MemTypeDefault, &memoryInfo), "CreateMemoryInfo");
        }
        finally
        {
            Marshal.FreeCoTaskMem(name);
        }

        try
        {
            nint allocator;
            Check(((delegate* unmanaged<nint, nint, nint*, nint>)Slot(ApiSlot.CreateAllocator))(
                session, memoryInfo, &allocator), "CreateAllocator");
            return allocator;
        }
        finally
        {
            ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseMemoryInfo))(memoryInfo);
        }
    }

    internal static void ReleaseAllocator(nint allocator) =>
        ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseAllocator))(allocator);

    internal static nint CreateTensor(nint allocator, long[] shape, int elementType)
    {
        nint value;
        fixed (long* shapePtr = shape)
        {
            Check(((delegate* unmanaged<nint, long*, nuint, int, nint*, nint>)Slot(
                ApiSlot.CreateTensorAsOrtValue))(allocator, shapePtr, (nuint)shape.Length, elementType,
                &value), "CreateTensorAsOrtValue");
        }

        return value;
    }

    internal static void ReleaseValue(nint value) =>
        ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseValue))(value);

    internal static nint GetTensorData(nint value)
    {
        nint data;
        Check(((delegate* unmanaged<nint, nint*, nint>)Slot(ApiSlot.GetTensorMutableData))(value, &data),
            "GetTensorMutableData");
        return data;
    }

    // Returns an ID3D12Resource. The caller owns the reference.
    internal static nint GetD3D12Resource(nint allocator, nint data)
    {
        nint resource;
        Check(((delegate* unmanaged<nint, nint, nint*, nint>)DmlFunction(DmlSlot.GetD3D12ResourceFromAllocation))(
            allocator, data, &resource), "GetD3D12ResourceFromAllocation");
        if (resource == 0)
        {
            throw new InvalidOperationException("DirectML returned no D3D12 resource for a tensor.");
        }

        return resource;
    }

    internal static nint CreateIoBinding(nint session)
    {
        nint binding;
        Check(((delegate* unmanaged<nint, nint*, nint>)Slot(ApiSlot.CreateIoBinding))(session, &binding),
            "CreateIoBinding");
        return binding;
    }

    internal static void ReleaseIoBinding(nint binding) =>
        ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseIoBinding))(binding);

    internal static void BindInput(nint binding, string name, nint value) =>
        BindTensor(ApiSlot.BindInput, binding, name, value);

    internal static void BindOutput(nint binding, string name, nint value) =>
        BindTensor(ApiSlot.BindOutput, binding, name, value);

    // Only the loop thread calls this.
    internal static void RunWithBinding(nint session, nint runOptions, nint binding) =>
        Check(((delegate* unmanaged<nint, nint, nint, nint>)Slot(ApiSlot.RunWithBinding))(
            session, runOptions, binding), "RunWithBinding");

    private static nint LoadApi()
    {
        nint apiBase = OrtGetApiBase();
        if (apiBase == 0)
        {
            throw new InvalidOperationException("onnxruntime.dll did not return an API base.");
        }

        // OrtApiBase::GetApi is the first member.
        nint api = ((delegate* unmanaged<uint, nint>)((nint*)apiBase)[0])(ApiVersion);
        if (api == 0)
        {
            throw new InvalidOperationException($"onnxruntime.dll does not provide C API version {ApiVersion}.");
        }

        return api;
    }

    private static nint DmlFunction(DmlSlot slot)
    {
        if (_dmlApi == 0)
        {
            nint name = Utf8("DML");
            try
            {
                nint api;
                Check(((delegate* unmanaged<nint, uint, nint*, nint>)Slot(ApiSlot.GetExecutionProviderApi))(
                    name, 0, &api), "GetExecutionProviderApi");
                if (api == 0)
                {
                    throw new InvalidOperationException("ONNX Runtime has no DirectML execution provider.");
                }

                _dmlApi = api;
            }
            finally
            {
                Marshal.FreeCoTaskMem(name);
            }
        }

        return ((nint*)_dmlApi)[(int)slot];
    }

    private static nint GetTypeInfo(nint session, bool input, int index)
    {
        nint typeInfo;
        ApiSlot slot = input ? ApiSlot.SessionGetInputTypeInfo : ApiSlot.SessionGetOutputTypeInfo;
        Check(((delegate* unmanaged<nint, nuint, nint*, nint>)Slot(slot))(session, (nuint)index, &typeInfo),
            slot.ToString());
        return typeInfo;
    }

    private static string GetIoName(nint session, bool input, int index)
    {
        nint allocator = DefaultAllocator();
        ApiSlot slot = input ? ApiSlot.SessionGetInputName : ApiSlot.SessionGetOutputName;
        nint name;
        Check(((delegate* unmanaged<nint, nuint, nint, nint*, nint>)Slot(slot))(
            session, (nuint)index, allocator, &name), slot.ToString());
        try
        {
            return Marshal.PtrToStringUTF8(name)
                ?? throw new InvalidOperationException("ONNX Runtime returned a null tensor name.");
        }
        finally
        {
            FreeWithAllocator(allocator, name);
        }
    }

    // The runtime owns the default allocator. Never release it.
    private static nint DefaultAllocator()
    {
        nint allocator;
        Check(((delegate* unmanaged<nint*, nint>)Slot(ApiSlot.GetAllocatorWithDefaultOptions))(&allocator),
            "GetAllocatorWithDefaultOptions");
        return allocator;
    }

    private static void FreeWithAllocator(nint allocator, nint pointer) =>
        Check(((delegate* unmanaged<nint, nint, nint>)Slot(ApiSlot.AllocatorFree))(allocator, pointer),
            "AllocatorFree");

    private static void BindTensor(ApiSlot slot, nint binding, string name, nint value)
    {
        nint namePtr = Utf8(name);
        try
        {
            Check(((delegate* unmanaged<nint, nint, nint, nint>)Slot(slot))(binding, namePtr, value),
                slot.ToString());
        }
        finally
        {
            Marshal.FreeCoTaskMem(namePtr);
        }
    }

    private static nint Slot(ApiSlot slot) => ((nint*)Api)[(int)slot];

    private static nint Utf8(string value) => Marshal.StringToCoTaskMemUTF8(value);

    private static void Check(nint status, string operation)
    {
        if (status == 0)
        {
            return;
        }

        string message = Marshal.PtrToStringUTF8(
            ((delegate* unmanaged<nint, nint>)Slot(ApiSlot.GetErrorMessage))(status)) ?? "no message";
        ((delegate* unmanaged<nint, void>)Slot(ApiSlot.ReleaseStatus))(status);
        throw new InvalidOperationException($"ONNX Runtime {operation} failed: {message}");
    }
}
