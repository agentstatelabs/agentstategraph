using System;
using System.IO;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace AgentStateGraph;

/// <summary>
/// Registers a <see cref="NativeLibrary.SetDllImportResolver"/> that
/// locates the <c>agentstategraph_ffi</c> shared library via, in order:
/// <list type="number">
///   <item>the <c>AGENTSTATEGRAPH_FFI_PATH</c> environment variable,</item>
///   <item>the directory alongside the managed assembly (NuGet
///   <c>runtimes/&lt;rid&gt;/native/</c> convention),</item>
///   <item>a <c>target/debug/</c> or <c>target/release/</c> directory
///   reachable by walking up from <see cref="AppContext.BaseDirectory"/>
///   (development convenience).</item>
/// </list>
/// The resolver is registered exactly once via a
/// <see cref="ModuleInitializerAttribute"/> on the first load of this
/// assembly. The P/Invoke layer (added in §2) uses the name
/// <c>agentstategraph_ffi</c> — matching the <c>Lib</c> constant — so
/// the resolver only kicks in for that one library name and leaves
/// everything else to the default .NET resolver.
/// </summary>
internal static class NativeLibraryLoader
{
    /// <summary>
    /// The library name that the P/Invoke layer passes to
    /// <c>[DllImport]</c>. Must match the <c>const string Lib</c> used
    /// by the Interop layer in §2.
    /// </summary>
    internal const string LibraryName = "agentstategraph_ffi";

    private static int _initialized;

    [ModuleInitializer]
    internal static void Initialize()
    {
        // Idempotent — ModuleInitializer runs once per module load, but
        // guard anyway so manual invocations (e.g. from tests) are safe.
        if (System.Threading.Interlocked.Exchange(ref _initialized, 1) != 0)
        {
            return;
        }

        NativeLibrary.SetDllImportResolver(
            typeof(NativeLibraryLoader).Assembly,
            Resolve);
    }

    /// <summary>
    /// The resolver delegate — see
    /// <see cref="DllImportResolver"/>.
    /// </summary>
    private static IntPtr Resolve(
        string libraryName,
        Assembly assembly,
        DllImportSearchPath? searchPath)
    {
        if (!string.Equals(libraryName, LibraryName, StringComparison.Ordinal))
        {
            // Not our library — fall through to the default resolver.
            return IntPtr.Zero;
        }

        string fileName = PlatformLibraryFileName();

        // 1. AGENTSTATEGRAPH_FFI_PATH environment override.
        string? envDir = Environment.GetEnvironmentVariable("AGENTSTATEGRAPH_FFI_PATH");
        if (!string.IsNullOrEmpty(envDir))
        {
            if (TryLoad(Path.Combine(envDir, fileName), out IntPtr handle))
            {
                return handle;
            }
        }

        // 2. Alongside the managed assembly (NuGet runtimes/<rid>/native/
        //    layout puts the native file next to the deployed assembly).
        string? assemblyDir = Path.GetDirectoryName(assembly.Location);
        if (!string.IsNullOrEmpty(assemblyDir))
        {
            if (TryLoad(Path.Combine(assemblyDir, fileName), out IntPtr handle))
            {
                return handle;
            }
        }

        // 3. Walk up from AppContext.BaseDirectory looking for a cargo
        //    target dir — dev convenience so `dotnet run` / `dotnet test`
        //    work straight out of a clone.
        if (TryFindCargoTarget(AppContext.BaseDirectory, fileName, out string? cargoPath))
        {
            if (TryLoad(cargoPath!, out IntPtr handle))
            {
                return handle;
            }
        }

        // Fall through to the default resolver (LD_LIBRARY_PATH /
        // DYLD_LIBRARY_PATH / PATH etc.).
        return IntPtr.Zero;
    }

    /// <summary>
    /// Platform-specific file name for the native FFI library.
    /// </summary>
    private static string PlatformLibraryFileName()
    {
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
        {
            return "agentstategraph_ffi.dll";
        }
        if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
        {
            return "libagentstategraph_ffi.dylib";
        }
        // Default to the Linux / Unix naming for everything else.
        return "libagentstategraph_ffi.so";
    }

    private static bool TryLoad(string path, out IntPtr handle)
    {
        if (File.Exists(path) && NativeLibrary.TryLoad(path, out handle))
        {
            return true;
        }
        handle = IntPtr.Zero;
        return false;
    }

    /// <summary>
    /// Walk up the directory tree from <paramref name="start"/> looking
    /// for a sibling <c>target/release/&lt;file&gt;</c> or
    /// <c>target/debug/&lt;file&gt;</c>. Release is preferred over debug
    /// when both exist.
    /// </summary>
    private static bool TryFindCargoTarget(
        string start,
        string fileName,
        out string? foundPath)
    {
        foundPath = null;
        DirectoryInfo? dir = new DirectoryInfo(start);
        // Cap the walk so a pathological layout can't spin forever.
        for (int i = 0; i < 16 && dir is not null; i++, dir = dir.Parent)
        {
            string release = Path.Combine(dir.FullName, "target", "release", fileName);
            if (File.Exists(release))
            {
                foundPath = release;
                return true;
            }
            string debug = Path.Combine(dir.FullName, "target", "debug", fileName);
            if (File.Exists(debug))
            {
                foundPath = debug;
                return true;
            }
        }
        return false;
    }
}
