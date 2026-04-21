using System.Runtime.CompilerServices;

// Exposes the internal NativeLibraryLoader (and the Interop.* helpers) to
// the xUnit project so §4 can test the resolver short-circuit without
// widening the public surface. The one exception to §4's "no main-project
// edits" rule, documented in the 0.7.25-beta.1 plan.
[assembly: InternalsVisibleTo("AgentStateGraph.Tests")]
