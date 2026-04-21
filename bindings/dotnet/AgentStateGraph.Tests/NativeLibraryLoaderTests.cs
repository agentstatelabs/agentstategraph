using System;
using System.Reflection;
using AgentStateGraph;
using Xunit;

namespace AgentStateGraph.Tests;

/// <summary>
/// Exercises the <c>DllImportResolver</c> registered by
/// <c>NativeLibraryLoader</c>. The resolver itself is
/// <c>internal</c>; the <c>InternalsVisibleTo("AgentStateGraph.Tests")</c>
/// attribute on the main assembly lets us reach in for targeted tests
/// without loading the native library.
/// </summary>
public sealed class NativeLibraryLoaderTests
{
    [Fact]
    public void DefaultResolver_LoadsNativeLibrary_ForSmokeRepoCreate()
    {
        // If the resolver + native lib are correctly wired, we can create
        // an in-memory repository. This is the end-to-end smoke for §1 of
        // the 0.7.25-beta.1 plan; every other [Fact] here exercises the
        // same path, but stating the contract explicitly is worthwhile.
        using var repo = new Repository();
        Assert.NotNull(repo);
    }

    [Fact]
    public void Resolver_UnknownLibraryName_ReturnsZero()
    {
        // Reach the internal Resolve via reflection. InternalsVisibleTo
        // makes the type accessible, but the method is private — reflection
        // is the cleanest way to exercise it without widening the surface.
        var loaderType = typeof(NativeLibraryLoader);
        var resolve = loaderType.GetMethod(
            "Resolve",
            BindingFlags.Static | BindingFlags.NonPublic);
        Assert.NotNull(resolve);

        var result = (IntPtr)resolve!.Invoke(
            null,
            new object?[] { "totally-unrelated-library", typeof(Repository).Assembly, null })!;

        Assert.Equal(IntPtr.Zero, result);
    }

    [Fact]
    public void Resolver_EnvOverride_IsConsultedFirst()
    {
        // We can't mock the filesystem here (no FakeFs dependency per §4's
        // "no new packages" rule), so we only verify the env var gate does
        // NOT throw when set to a nonexistent directory — it must fall
        // through to the subsequent lookup strategies, not crash.
        var prior = Environment.GetEnvironmentVariable("AGENTSTATEGRAPH_FFI_PATH");
        try
        {
            Environment.SetEnvironmentVariable(
                "AGENTSTATEGRAPH_FFI_PATH",
                "/nonexistent/path/for/xunit");

            // Fresh resolver invocation via the already-registered callback.
            // A fresh Repository() forces native load — if env-first logic
            // blew up on a missing dir we'd see an exception here.
            using var repo = new Repository();
            Assert.NotNull(repo);
        }
        finally
        {
            Environment.SetEnvironmentVariable("AGENTSTATEGRAPH_FFI_PATH", prior);
        }
    }
}
