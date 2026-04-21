using System;
using System.Runtime.InteropServices;

namespace AgentStateGraph.Interop;

/// <summary>
/// <see cref="SafeHandle"/> wrapper for an <c>SgRepo</c> opaque pointer.
/// Freed via <c>agentstategraph_free</c> on collection / disposal.
/// </summary>
internal sealed class SafeRepoHandle : SafeHandle
{
    /// <summary>Creates an empty (invalid) handle — used by the runtime.</summary>
    public SafeRepoHandle()
        : base(IntPtr.Zero, ownsHandle: true)
    {
    }

    /// <summary>
    /// Creates a handle adopting an already-allocated raw pointer. The
    /// wrapper takes ownership and will free on disposal / finalization.
    /// </summary>
    public SafeRepoHandle(IntPtr raw)
        : base(IntPtr.Zero, ownsHandle: true)
    {
        SetHandle(raw);
    }

    public override bool IsInvalid => handle == IntPtr.Zero;

    /// <summary>
    /// Factory mirror of the <see cref="SafeRepoHandle(IntPtr)"/>
    /// constructor — lets callers write <c>SafeRepoHandle.Adopt(ptr)</c>
    /// at call sites where <c>new</c> would read awkwardly.
    /// </summary>
    public static SafeRepoHandle Adopt(IntPtr raw) => new SafeRepoHandle(raw);

    protected override bool ReleaseHandle()
    {
        if (handle != IntPtr.Zero)
        {
            NativeMethods.agentstategraph_free(handle);
        }
        return true;
    }
}

/// <summary>
/// <see cref="SafeHandle"/> wrapper for an <c>SgTaskStore</c> opaque pointer.
/// Freed via <c>agentstategraph_taskstore_free</c> on collection / disposal.
/// </summary>
internal sealed class SafeTaskStoreHandle : SafeHandle
{
    public SafeTaskStoreHandle()
        : base(IntPtr.Zero, ownsHandle: true)
    {
    }

    public SafeTaskStoreHandle(IntPtr raw)
        : base(IntPtr.Zero, ownsHandle: true)
    {
        SetHandle(raw);
    }

    public override bool IsInvalid => handle == IntPtr.Zero;

    public static SafeTaskStoreHandle Adopt(IntPtr raw) => new SafeTaskStoreHandle(raw);

    protected override bool ReleaseHandle()
    {
        if (handle != IntPtr.Zero)
        {
            NativeMethods.agentstategraph_taskstore_free(handle);
        }
        return true;
    }
}

/// <summary>
/// <see cref="SafeHandle"/> wrapper for an <c>SgPolicyStore</c> opaque
/// pointer. Freed via <c>agentstategraph_policy_store_free</c> on
/// collection / disposal.
/// </summary>
internal sealed class SafePolicyStoreHandle : SafeHandle
{
    public SafePolicyStoreHandle()
        : base(IntPtr.Zero, ownsHandle: true)
    {
    }

    public SafePolicyStoreHandle(IntPtr raw)
        : base(IntPtr.Zero, ownsHandle: true)
    {
        SetHandle(raw);
    }

    public override bool IsInvalid => handle == IntPtr.Zero;

    public static SafePolicyStoreHandle Adopt(IntPtr raw) => new SafePolicyStoreHandle(raw);

    protected override bool ReleaseHandle()
    {
        if (handle != IntPtr.Zero)
        {
            NativeMethods.agentstategraph_policy_store_free(handle);
        }
        return true;
    }
}
