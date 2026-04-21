using System;
using System.Runtime.InteropServices;

namespace AgentStateGraph.Interop;

/// <summary>
/// Helpers around the <c>agentstategraph_free_string</c> "C allocates, C
/// frees" convention used by every string-returning ABI function.
/// </summary>
internal static class Strings
{
    /// <summary>
    /// Reads a UTF-8 C string from <paramref name="ptr"/>, frees it via
    /// <c>agentstategraph_free_string</c>, and returns the managed copy.
    /// Returns <c>null</c> when <paramref name="ptr"/> is
    /// <see cref="IntPtr.Zero"/>.
    /// </summary>
    internal static string? ConsumeUtf8(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero)
        {
            return null;
        }
        try
        {
            return Marshal.PtrToStringUTF8(ptr);
        }
        finally
        {
            NativeMethods.agentstategraph_free_string(ptr);
        }
    }

    /// <summary>
    /// Frees a C-allocated UTF-8 string without decoding it. Safe on
    /// <see cref="IntPtr.Zero"/>.
    /// </summary>
    internal static void FreeUtf8(IntPtr ptr)
    {
        if (ptr != IntPtr.Zero)
        {
            NativeMethods.agentstategraph_free_string(ptr);
        }
    }
}
