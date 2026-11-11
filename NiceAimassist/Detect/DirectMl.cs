using System.Runtime.InteropServices;
using NiceAimassist.Display;
using Silk.NET.Direct3D12;

namespace NiceAimassist.Detect;

internal static unsafe partial class DirectMl
{
    // The IID of IDMLDevice.
    private static readonly Guid DeviceIid = new("6dbd6437-96fd-423f-a98c-ae5e7c2a573f");

    [LibraryImport("DirectML.dll")]
    private static partial int DMLCreateDevice(ID3D12Device* d3d12Device, uint flags, Guid* riid, void** device);

    /// <summary>Returns an IDMLDevice. The caller releases it.</summary>
    internal static void* CreateDevice(ID3D12Device* device)
    {
        void* result;
        Guid iid = DeviceIid;
        Com.Check(DMLCreateDevice(device, 0, &iid, &result), "DMLCreateDevice");
        return result;
    }
}
