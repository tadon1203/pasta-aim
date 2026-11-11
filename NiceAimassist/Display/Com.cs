using System.Runtime.InteropServices;
using Silk.NET.Core.Native;

namespace NiceAimassist.Display;

internal static unsafe class Com
{
    internal const int DxgiNotFound = unchecked((int)0x887A0002);
    internal const int DxgiWasStillDrawing = unchecked((int)0x887A000A);
    internal const int DxgiAccessLost = unchecked((int)0x887A0026);
    internal const int DxgiWaitTimeout = unchecked((int)0x887A0027);

    internal const uint GenericAll = 0x10000000;
    internal const uint SharedResourceReadWrite = 0x80000001;

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
