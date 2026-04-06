export class StateGraph {
  /** Create a new StateGraph. Pass a path for SQLite, or omit for in-memory. */
  constructor(path?: string);

  /** Get a value at a path. */
  get(path: string, reference?: string): any;

  /** Set a value at a path, creating a commit. */
  set(path: string, value: any, description: string, reference?: string, category?: string, agent?: string, reasoning?: string, confidence?: number, tags?: string[]): string;

  /** Set a JSON value at a path, creating a commit. */
  setJson(path: string, value: any, description: string, reference?: string, category?: string, agent?: string, reasoning?: string, confidence?: number, tags?: string[]): string;

  /** Delete a value at a path, creating a commit. */
  delete(path: string, description: string, reference?: string, category?: string): string;

  /** Create a branch from a ref. */
  branch(name: string, from?: string): string;

  /** Delete a branch. */
  deleteBranch(name: string): boolean;

  /** List branches. */
  listBranches(prefix?: string): Array<{ name: string; id: string }>;

  /** Merge source branch into target. */
  merge(source: string, target?: string, description?: string, reasoning?: string): string;

  /** Structured diff between two refs. */
  diff(refA: string, refB: string): any[];

  /** Commit log from a ref. */
  log(reference?: string, limit?: number): any[];

  /** Create a speculation. Returns handle ID. */
  speculate(from?: string, label?: string): number;

  /** Get a value from a speculation. */
  specGet(handleId: number, path: string): any;

  /** Set a value within a speculation. */
  specSet(handleId: number, path: string, value: any): void;

  /** Commit a speculation to its base branch. */
  commitSpeculation(handleId: number, description: string, category?: string, reasoning?: string, confidence?: number): string;

  /** Discard a speculation. */
  discardSpeculation(handleId: number): void;
}
