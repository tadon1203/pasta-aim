using System.Runtime.InteropServices;
using Silk.NET.Core.Native;

namespace Pstm.Shared.Gpu;

internal static unsafe class ComInterop
{
    internal static void Check(int result, string operation)
    {
        if (result < 0)
        {
            throw new InvalidOperationException(
                $"{operation} failed (0x{result:X8}).", Marshal.GetExceptionForHR(result));
        }
    }

    internal static void Release<T>(ref T* pointer)
        where T : unmanaged
    {
        if (pointer != null)
        {
            ((IUnknown*)pointer)->Release();
            pointer = null;
        }
    }

    /// <summary>Returns a new reference. The caller releases it.</summary>
    internal static T* Query<T>(void* source)
        where T : unmanaged, IComVtbl<T>
    {
        void* result;
        Check(((IUnknown*)source)->QueryInterface(SilkMarshal.GuidPtrOf<T>(), &result), $"QueryInterface {typeof(T).Name}");
        return (T*)result;
    }

    internal static Guid* IidOf<T>()
        where T : unmanaged, IComVtbl<T> => SilkMarshal.GuidPtrOf<T>();
}
