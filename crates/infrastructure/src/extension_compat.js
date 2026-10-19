// ---- compatibility: the parts of the API that extensions touch while they start ------------------
// Part of the extension host script (put in at `// @@COMPAT@@`). Much of what real extensions
// ask for in `activate` is small: a translated string, the list of installed extensions, a folder
// to keep data in, a file system. What RustCode has no window for (webviews, notebooks, tests,
// chat) is accepted and left out, so that the extension's other features still work.

const os = require('os');
const fsPromises = require('fs').promises;
const fsSync = require('fs');

// `SlowBuffer` is gone from newer Node versions, and old packages (buffer-equal-constant-time)
// still patch its prototype.
{
  const buffer = require('buffer');
  if (!buffer.SlowBuffer) {
    buffer.SlowBuffer = function SlowBuffer(size) {
      return Buffer.allocUnsafeSlow(size);
    };
    buffer.SlowBuffer.prototype = Buffer.prototype;
  }
}

// ---- l10n ---------------------------------------------------------------------------------------

const l10n = {
  // `t('Hello {0}', name)`, `t('Hello {name}', { name })`, or `t({ message, args, comment })`.
  t(message, ...args) {
    let text = message;
    let values = args;
    if (message && typeof message === 'object') {
      text = message.message;
      values = Array.isArray(message.args) ? message.args : message.args ? [message.args] : [];
    }
    if (values.length === 1 && values[0] && typeof values[0] === 'object' && !Array.isArray(values[0])) {
      const named = values[0];
      return String(text).replace(/\{(\w+)\}/g, (match, key) => (key in named ? String(named[key]) : match));
    }
    if (values.length === 1 && Array.isArray(values[0])) values = values[0];
    return String(text).replace(/\{(\d+)\}/g, (match, index) => (values[Number(index)] !== undefined ? String(values[Number(index)]) : match));
  },
  bundle: undefined,
  uri: undefined,
};

// ---- extensions ---------------------------------------------------------------------------------

const extensionObjects = new Map(); // lower-case id -> object

function extensionObject(info) {
  const key = info.id.toLowerCase();
  if (extensionObjects.has(key)) return extensionObjects.get(key);
  let manifest;
  const object = {
    id: info.id,
    extensionUri: Uri.file(info.path),
    extensionPath: info.path,
    extensionKind: 1,
    get isActive() {
      return !!extensions.get(info.id)?.done;
    },
    get packageJSON() {
      if (!manifest) {
        try {
          manifest = JSON.parse(fsSync.readFileSync(path.join(info.path, 'package.json'), 'utf8'));
        } catch (error) {
          manifest = {};
        }
      }
      return manifest;
    },
    get exports() {
      return extensions.get(info.id)?.api;
    },
    activate: async () => {
      await activate(info.id);
      return extensions.get(info.id)?.api;
    },
  };
  extensionObjects.set(key, object);
  return object;
}

// The git extension that ships with VS Code. RustCode does its own git, so this stands in for the
// API other extensions (git history, graph) ask it for: git's path and state, and no repositories.
let builtinGit;
function builtinGitExtension() {
  if (builtinGit) return builtinGit;
  const stateChanged = new EventEmitter();
  const api = {
    state: 'initialized',
    onDidChangeState: (listener, thisArg, disposables) => stateChanged.event(listener, thisArg, disposables),
    onDidPublish: neverFires,
    onDidOpenRepository: neverFires,
    onDidCloseRepository: neverFires,
    get git() {
      let found = 'git';
      try {
        const lookup = require('child_process').spawnSync(process.platform === 'win32' ? 'where' : 'which', ['git'], { encoding: 'utf8' });
        if (lookup.status === 0 && lookup.stdout.trim()) found = lookup.stdout.trim().split(/\r?\n/)[0];
      } catch (error) {
        // `git` is looked up on the PATH.
      }
      return { path: found };
    },
    repositories: [],
    getRepository: () => null,
    getRepositoryRoot: async () => null,
    init: async () => null,
    openRepository: async () => null,
    registerCredentialsProvider: () => new Disposable(() => {}),
    registerPushErrorHandler: () => new Disposable(() => {}),
    registerRemoteSourceProvider: () => new Disposable(() => {}),
    registerPostCommitCommandsProvider: () => new Disposable(() => {}),
    registerBranchProtectionProvider: () => new Disposable(() => {}),
    toGitUri: (uri, ref) => uri.with({ scheme: 'git', query: JSON.stringify({ path: uri.fsPath, ref }) }),
  };
  const exports = { enabled: true, onDidChangeEnablement: neverFires, getAPI: () => api };
  builtinGit = {
    id: 'vscode.git',
    extensionUri: Uri.file(process.cwd()),
    extensionPath: process.cwd(),
    extensionKind: 1,
    isActive: true,
    isBuiltin: true,
    packageJSON: { name: 'git', publisher: 'vscode', version: '1.0.0', isBuiltin: true },
    exports,
    activate: async () => exports,
  };
  return builtinGit;
}

const extensionsApi = {
  getExtension: (id) => {
    const entry = [...extensions.values()].find((e) => e.info.id.toLowerCase() === String(id).toLowerCase());
    if (!entry && String(id).toLowerCase() === 'vscode.git') return builtinGitExtension();
    return entry ? extensionObject(entry.info) : undefined;
  },
  get all() {
    return [...extensions.values()].map((e) => extensionObject(e.info));
  },
  // The extensions of the other hosts (remote, web): there are none.
  allAcrossExtensionHosts: [],
  onDidChange: neverFires,
};

// ---- the context of an extension --------------------------------------------------------------

function dataFolder(kind, id) {
  const dir = path.join(os.homedir(), '.rustcode', 'extension-data', kind, id);
  try {
    fsSync.mkdirSync(dir, { recursive: true });
  } catch (error) {
    // The extension finds out when it writes.
  }
  return dir;
}

function environmentCollection() {
  const entries = new Map();
  const collection = {
    persistent: true,
    description: undefined,
    replace: (name, value, options) => entries.set(name, { type: 1, value, options }),
    append: (name, value, options) => entries.set(name, { type: 2, value, options }),
    prepend: (name, value, options) => entries.set(name, { type: 3, value, options }),
    get: (name) => entries.get(name),
    forEach: (callback, thisArg) => entries.forEach((mutator, name) => callback.call(thisArg, name, mutator, collection)),
    delete: (name) => entries.delete(name),
    clear: () => entries.clear(),
    getScoped: () => collection,
    [Symbol.iterator]: () => entries.entries(),
  };
  return collection;
}

function makeContext(info) {
  const globalStorage = dataFolder('global', info.id);
  const logs = dataFolder('logs', info.id);
  const workspaceKey = workspaceFolders[0] ? Buffer.from(workspaceFolders[0].uri.fsPath).toString('hex').slice(0, 24) : 'none';
  const storage = workspaceFolders[0] ? dataFolder('workspace', path.join(info.id, workspaceKey)) : undefined;
  const secretChanged = new EventEmitter();
  const secrets = new Map();
  return {
    subscriptions: [],
    extensionPath: info.path,
    extensionUri: Uri.file(info.path),
    extensionMode: 1,
    asAbsolutePath: (relative) => path.join(info.path, relative),
    globalState: memento(),
    workspaceState: memento(),
    secrets: {
      get: async (key) => secrets.get(key),
      store: async (key, value) => {
        secrets.set(key, value);
        secretChanged.fire({ key });
      },
      delete: async (key) => {
        secrets.delete(key);
        secretChanged.fire({ key });
      },
      onDidChange: (l, t, d) => secretChanged.event(l, t, d),
    },
    environmentVariableCollection: environmentCollection(),
    extension: extensionObject(info),
    globalStorageUri: Uri.file(globalStorage),
    globalStoragePath: globalStorage,
    storageUri: storage ? Uri.file(storage) : undefined,
    storagePath: storage,
    logUri: Uri.file(logs),
    logPath: logs,
    languageModelAccessInformation: { onDidChange: neverFires, canSendRequest: () => undefined },
  };
}

// ---- workspace.fs and findFiles -------------------------------------------------------------------

const fileTypeOf = (stats) => (stats.isSymbolicLink() ? 64 : stats.isDirectory() ? 2 : stats.isFile() ? 1 : 0);

// A failed call of the file system as the error VS Code gives for it.
function fileSystemError(error, uri) {
  const text = uri ? uri.toString() : '';
  switch (error && error.code) {
    case 'ENOENT': return FileSystemError.FileNotFound(text);
    case 'EEXIST': return FileSystemError.FileExists(text);
    case 'ENOTDIR': return FileSystemError.FileNotADirectory(text);
    case 'EISDIR': return FileSystemError.FileIsADirectory(text);
    case 'EACCES':
    case 'EPERM': return FileSystemError.NoPermissions(text);
    default: return error;
  }
}

const workspaceFs = {
  async stat(uri) {
    try {
      const link = await fsPromises.lstat(uri.fsPath);
      const stats = link.isSymbolicLink() ? await fsPromises.stat(uri.fsPath).catch(() => link) : link;
      return { type: fileTypeOf(link) | (link.isSymbolicLink() ? 0 : 0) || fileTypeOf(stats), ctime: stats.ctimeMs, mtime: stats.mtimeMs, size: stats.size };
    } catch (error) {
      throw fileSystemError(error, uri);
    }
  },
  async readFile(uri) {
    try {
      return new Uint8Array(await fsPromises.readFile(uri.fsPath));
    } catch (error) {
      throw fileSystemError(error, uri);
    }
  },
  async writeFile(uri, content) {
    try {
      await fsPromises.writeFile(uri.fsPath, content);
    } catch (error) {
      throw fileSystemError(error, uri);
    }
  },
  async readDirectory(uri) {
    try {
      const entries = await fsPromises.readdir(uri.fsPath, { withFileTypes: true });
      return entries.map((e) => [e.name, e.isSymbolicLink() ? 64 : e.isDirectory() ? 2 : e.isFile() ? 1 : 0]);
    } catch (error) {
      throw fileSystemError(error, uri);
    }
  },
  async createDirectory(uri) {
    try {
      await fsPromises.mkdir(uri.fsPath, { recursive: true });
    } catch (error) {
      throw fileSystemError(error, uri);
    }
  },
  async delete(uri, options) {
    try {
      await fsPromises.rm(uri.fsPath, { recursive: !!(options && options.recursive), force: false });
    } catch (error) {
      throw fileSystemError(error, uri);
    }
  },
  async rename(from, to, options) {
    try {
      if (!(options && options.overwrite) && fsSync.existsSync(to.fsPath)) throw Object.assign(new Error('exists'), { code: 'EEXIST' });
      await fsPromises.rename(from.fsPath, to.fsPath);
    } catch (error) {
      throw fileSystemError(error, to);
    }
  },
  async copy(from, to, options) {
    try {
      if (!(options && options.overwrite) && fsSync.existsSync(to.fsPath)) throw Object.assign(new Error('exists'), { code: 'EEXIST' });
      await fsPromises.cp(from.fsPath, to.fsPath, { recursive: true });
    } catch (error) {
      throw fileSystemError(error, to);
    }
  },
  isWritableFileSystem: (scheme) => (scheme === 'file' || scheme === 'untitled' ? true : undefined),
};

const skippedFolders = new Set(['.git', 'node_modules', '.hg', '.svn']);

// Files of the workspace folders that match `include` and not `exclude` (globs, or a RelativePattern).
async function findFiles(include, exclude, maxResults, token) {
  const limit = maxResults ?? 20000;
  const patternOf = (pattern) => (pattern && typeof pattern === 'object' ? { base: pattern.baseUri ? pattern.baseUri.fsPath : pattern.base, glob: pattern.pattern } : { base: undefined, glob: pattern });
  const wanted = patternOf(include);
  const skipped = exclude === null ? undefined : patternOf(exclude);
  const bases = wanted.base ? [wanted.base] : workspaceFolders.map((f) => f.uri.fsPath);
  const found = [];
  async function walk(dir, root) {
    if (found.length >= limit || (token && token.isCancellationRequested)) return;
    let entries;
    try {
      entries = await fsPromises.readdir(dir, { withFileTypes: true });
    } catch (error) {
      return;
    }
    for (const entry of entries) {
      if (found.length >= limit) return;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (skippedFolders.has(entry.name)) continue;
        await walk(full, root);
      } else if (entry.isFile() || entry.isSymbolicLink()) {
        const relative = path.relative(root, full).split(path.sep).join('/');
        if (!globMatches(wanted.glob, relative)) continue;
        if (skipped && skipped.glob && globMatches(skipped.glob, path.relative(skipped.base || root, full).split(path.sep).join('/'))) continue;
        found.push(Uri.file(full));
      }
    }
  }
  for (const base of bases) await walk(base, base);
  return found;
}

// ---- namespaces RustCode has no window for -----------------------------------------------------
// What an extension registers there is accepted and never used; what it asks for gets an empty answer.

// An object that takes `register...`, `create...` and `on...` calls and does nothing with them.
function looseObject(label, own) {
  const target = { ...(own || {}) };
  return new Proxy(target, {
    get(object, key) {
      if (key in object) return object[key];
      if (typeof key === 'symbol' || key === 'then' || key === 'toJSON') return undefined;
      if (/^on[A-Z]/.test(key)) return neverFires;
      if (/^register[A-Z]/.test(key)) return () => new Disposable(() => {});
      if (/^create[A-Z]/.test(key)) return () => looseObject(`${label}.${key}`);
      if (key === 'dispose') return () => {};
      return undefined;
    },
  });
}

class FileDecoration {
  constructor(badge, tooltip, color) {
    this.badge = badge;
    this.tooltip = tooltip;
    this.color = color;
    this.propagate = false;
  }
}
class TestTag {
  constructor(id) {
    this.id = id;
  }
}
class TestMessage {
  constructor(message) {
    this.message = message;
  }
  static diff(message, expected, actual) {
    return Object.assign(new TestMessage(message), { expectedOutput: expected, actualOutput: actual });
  }
}
class TestRunProfileKind {}
const TestRunProfileKindEnum = { Run: 1, Debug: 2, Coverage: 3 };
class TestRunRequest {
  constructor(include, exclude, profile, continuous) {
    Object.assign(this, { include, exclude, profile, continuous });
  }
}

function testItemCollection() {
  const items = new Map();
  return {
    get size() { return items.size; },
    replace: (list) => { items.clear(); for (const item of list) items.set(item.id, item); },
    forEach: (callback, thisArg) => items.forEach((item) => callback.call(thisArg, item, this)),
    add: (item) => items.set(item.id, item),
    delete: (id) => items.delete(id),
    get: (id) => items.get(id),
    [Symbol.iterator]: () => items.entries(),
  };
}

const tests = {
  createTestController(id, label) {
    return {
      id,
      label,
      items: testItemCollection(),
      refreshHandler: undefined,
      resolveHandler: undefined,
      createRunProfile: (profileLabel, kind) => ({ label: profileLabel, kind, isDefault: false, tag: undefined, dispose() {}, onDidChangeDefault: neverFires, runHandler() {} }),
      createTestItem: (itemId, itemLabel, uri) => ({ id: itemId, label: itemLabel, uri, children: testItemCollection(), parent: undefined, tags: [], canResolveChildren: false, busy: false, range: undefined, error: undefined }),
      createTestRun: () => ({ name: undefined, token: neverCancelled(), isPersisted: false, enqueued() {}, started() {}, skipped() {}, failed() {}, errored() {}, passed() {}, appendOutput() {}, addCoverage() {}, end() {}, onDidDispose: neverFires }),
      invalidateTestResults() {},
      dispose() {},
    };
  },
  registerTestFollower: () => new Disposable(() => {}),
  runTests: async () => {},
  testResults: [],
  onDidChangeTestResults: neverFires,
};

const authentication = {
  getSession: async (provider, scopes, options) => {
    if (options && options.createIfNone) throw new Error('RustCode cannot sign in to extension services yet');
    return undefined;
  },
  getAccounts: async () => [],
  registerAuthenticationProvider: () => new Disposable(() => {}),
  onDidChangeSessions: neverFires,
};

const debugApi = looseObject('vscode.debug', {
  activeDebugSession: undefined,
  activeDebugConsole: { append() {}, appendLine() {} },
  activeStackItem: undefined,
  breakpoints: [],
  startDebugging: async () => false,
  stopDebugging: async () => {},
  addBreakpoints() {},
  removeBreakpoints() {},
  asDebugSourceUri: (source) => Uri.parse(`debug:${source && source.path ? source.path : ''}`),
});

const notebooks = looseObject('vscode.notebooks', {
  createRendererMessaging: () => ({ onDidReceiveMessage: neverFires, postMessage: async () => false }),
  createNotebookController: (id, type, label) => ({ id, notebookType: type, label, supportedLanguages: undefined, dispose() {}, createNotebookCellExecution: () => ({ start() {}, end() {}, clearOutput: async () => {}, replaceOutput: async () => {}, appendOutput: async () => {} }), onDidChangeSelectedNotebooks: neverFires, updateNotebookAffinity() {} }),
});

const comments = looseObject('vscode.comments', {
  createCommentController: (id, label) => ({ id, label, options: undefined, commentingRangeProvider: undefined, createCommentThread: () => ({ dispose() {} }), dispose() {} }),
});

const chat = looseObject('vscode.chat', {
  createChatParticipant: (id) => ({ id, iconPath: undefined, requestHandler: undefined, followupProvider: undefined, onDidReceiveFeedback: neverFires, dispose() {} }),
});

const lm = looseObject('vscode.lm', {
  selectChatModels: async () => [],
  tools: [],
  registerTool: () => new Disposable(() => {}),
  onDidChangeChatModels: neverFires,
});

const telemetryLogger = () => ({ logUsage() {}, logError() {}, dispose() {}, onDidChangeEnableStates: neverFires, isUsageEnabled: false, isErrorsEnabled: false });

// ---- commands of the editor itself -------------------------------------------------------------
// `executeCommand` of one of these goes to RustCode, which knows what to do (or ignores it).

const editorCommands = new Set([
  'setContext', 'vscode.open', 'vscode.openFolder', 'vscode.diff', 'revealFileInOS', 'workbench.action.openSettings', 'workbench.action.openSettingsJson',
  'workbench.view.explorer', 'workbench.view.scm', 'workbench.action.reloadWindow', 'workbench.action.files.save', 'workbench.action.files.saveAll',
  'workbench.action.focusActiveEditorGroup', 'workbench.action.terminal.focus', 'workbench.action.output.toggleOutput', 'workbench.action.closeActiveEditor',
  'workbench.action.showCommands', 'workbench.action.quickOpen', 'workbench.actions.view.problems', 'editor.action.formatDocument', 'editor.action.rename',
  'editor.action.revealDefinition', 'editor.action.goToReferences', 'editor.action.quickFix', 'workbench.action.tasks.runTask', 'workbench.action.tasks.build', 'workbench.action.tasks.test',
  'workbench.action.debug.start', 'workbench.action.debug.stop', 'workbench.action.debug.continue',
]);

// Commands of the editor with these starts exist in VS Code, and ask RustCode: those it has no
// use for are ignored there, so that an extension that runs one carries on.
const editorCommandPrefixes = ['workbench.', 'testing.', 'editor.', 'list.', 'notifications.', 'vscode.', 'git.', 'scm.', 'search.', 'explorer.', 'breadcrumbs.', 'settings.', 'outline.', 'debug.'];

function forwardEditorCommand(id, args) {
  // Every view that is contributed has a command that shows it.
  if (id.endsWith('.focus') && viewOwner.has(id.slice(0, -'.focus'.length))) {
    send({ type: 'editorCommand', command: 'rustcode.focusView', arguments: [id.slice(0, -'.focus'.length)] });
    return true;
  }
  if (!editorCommands.has(id) && !editorCommandPrefixes.some((p) => id.startsWith(p))) return undefined;
  send({ type: 'editorCommand', command: id, arguments: JSON.parse(JSON.stringify(args, (k, v) => (v instanceof Uri ? v.toJSON() : v))) });
  return true;
}

const IndentAction = { None: 0, Indent: 1, IndentOutdent: 2, Outdent: 3 };

class DocumentDropOrPasteEditKind {
  constructor(value) {
    this.value = value;
  }
  append(...parts) {
    return new DocumentDropOrPasteEditKind([this.value, ...parts].filter(Boolean).join('.'));
  }
  intersects(other) {
    return this.contains(other) || other.contains(this);
  }
  contains(other) {
    return this.value === '' || other.value === this.value || other.value.startsWith(this.value + '.');
  }
}
DocumentDropOrPasteEditKind.Empty = new DocumentDropOrPasteEditKind('');
DocumentDropOrPasteEditKind.Text = new DocumentDropOrPasteEditKind('text');
DocumentDropOrPasteEditKind.TextUpdateImports = new DocumentDropOrPasteEditKind('text.updateImports');

class DocumentPasteEdit {
  constructor(insertText, title, kind) {
    Object.assign(this, { insertText, title, kind });
  }
}
class DocumentDropEdit {
  constructor(insertText, title, kind) {
    Object.assign(this, { insertText, title, kind });
  }
}
class DataTransferItem {
  constructor(value) {
    this.value = value;
  }
  async asString() {
    return String(this.value);
  }
  asFile() {
    return undefined;
  }
}
class DataTransfer {
  #items = new Map();
  get(mime) {
    return this.#items.get(mime);
  }
  set(mime, item) {
    this.#items.set(mime, item);
  }
  forEach(callback, thisArg) {
    this.#items.forEach((item, mime) => callback.call(thisArg, item, mime, this));
  }
  [Symbol.iterator]() {
    return this.#items.entries();
  }
}

const NotebookCellKind = { Markup: 1, Code: 2 };
class NotebookCellOutputItem {
  constructor(data, mime) {
    this.data = data;
    this.mime = mime;
  }
  static text(value, mime = 'text/plain') {
    return new NotebookCellOutputItem(Buffer.from(String(value)), mime);
  }
  static json(value, mime = 'application/json') {
    return new NotebookCellOutputItem(Buffer.from(JSON.stringify(value)), mime);
  }
  static stdout(value) {
    return NotebookCellOutputItem.text(value, 'application/vnd.code.notebook.stdout');
  }
  static stderr(value) {
    return NotebookCellOutputItem.text(value, 'application/vnd.code.notebook.stderr');
  }
  static error(error) {
    return NotebookCellOutputItem.json({ name: error.name, message: error.message, stack: error.stack }, 'application/vnd.code.notebook.error');
  }
}
class NotebookCellOutput {
  constructor(items, metadata) {
    this.items = items;
    this.metadata = metadata;
  }
}
class NotebookCellData {
  constructor(kind, value, languageId) {
    Object.assign(this, { kind, value, languageId });
  }
}
class NotebookData {
  constructor(cells) {
    this.cells = cells;
  }
}
class NotebookRange {
  constructor(start, end) {
    Object.assign(this, { start, end });
  }
}
class NotebookEdit {
  static replaceCells(range, cells) {
    return Object.assign(new NotebookEdit(), { range, newCells: cells });
  }
  static insertCells(index, cells) {
    return NotebookEdit.replaceCells(new NotebookRange(index, index), cells);
  }
  static deleteCells(range) {
    return NotebookEdit.replaceCells(range, []);
  }
}

// Enumerations of parts of the API that RustCode has no window for.
const compatEnums = {
  CommentMode: { Editing: 0, Preview: 1 },
  CommentThreadCollapsibleState: { Collapsed: 0, Expanded: 1 },
  CommentThreadState: { Unresolved: 0, Resolved: 1 },
  NotebookControllerAffinity: { Default: 1, Preferred: 2 },
  NotebookEditorRevealType: { Default: 0, InCenter: 1, InCenterIfOutsideViewport: 2, AtTop: 3 },
  NotebookCellStatusBarAlignment: { Left: 1, Right: 2 },
  PortAutoForwardAction: { Notify: 1, OpenBrowser: 2, OpenPreview: 3, Silent: 4, Ignore: 5, OpenBrowserOnce: 6 },
  ExternalUriOpenerPriority: { None: 0, Option: 1, Default: 2, Preferred: 3 },
  DebugConsoleMode: { Separate: 0, MergeWithParent: 1 },
  DebugConfigurationProviderTriggerKind: { Initial: 1, Dynamic: 2 },
  StandardTokenType: { Other: 0, Comment: 1, String: 2, RegEx: 3 },
  ChatResultFeedbackKind: { Unhelpful: 0, Helpful: 1 },
  SourceControlInputBoxValidationType: { Error: 0, Warning: 1, Information: 2 },
  QuickInputButtons: { Back: { iconPath: new ThemeIcon('arrow-left'), tooltip: 'Back' } },
  ChatLocation: { Panel: 1, Terminal: 2, Notebook: 3, Editor: 4 },
};

// A class of the API that RustCode has no use for. It can be extended and made, so that a program
// that only names it at load time (`class X extends vscode.FileCoverage`) starts.
function lenientClass(name) {
  const stub = class {
    constructor(...args) {
      this.args = args;
    }
  };
  Object.defineProperty(stub, 'name', { value: name });
  return stub;
}

const compatExports = {
  ...compatEnums,
  IndentAction,
  DocumentDropOrPasteEditKind,
  DocumentPasteEdit,
  DocumentDropEdit,
  DataTransfer,
  DataTransferItem,
  NotebookCellKind,
  NotebookCellOutputItem,
  NotebookCellOutput,
  NotebookCellData,
  NotebookData,
  NotebookRange,
  NotebookEdit,
  l10n,
  authentication,
  debug: debugApi,
  notebooks,
  comments,
  chat,
  lm,
  tests,
  FileDecoration,
  TestTag,
  TestMessage,
  TestRunRequest,
  TestRunProfileKind: TestRunProfileKindEnum,
};

Object.assign(window, {
  registerUriHandler: () => new Disposable(() => {}),
  registerTerminalProfileProvider: () => new Disposable(() => {}),
  registerFileDecorationProvider: () => new Disposable(() => {}),
  onDidChangeWindowState: neverFires,
});
Object.assign(workspace, { fs: workspaceFs, findFiles });
Object.assign(languages, { setLanguageConfiguration: () => new Disposable(() => {}) });

// Every class of the API has to be a real member of the module: programs that bundle copy it, and
// what is not there at the time is lost. The ones nobody modelled become lenient classes.
const moreApiClasses = `
  FileCoverage StatementCoverage BranchCoverage DeclarationCoverage TestCoverageCount TabInputText TabInputTextDiff TabInputCustom TabInputWebview
  TabInputNotebook TabInputNotebookDiff TabInputTerminal LanguageModelChatMessage LanguageModelChatMessageRole LanguageModelTextPart
  LanguageModelToolResult LanguageModelToolResultPart LanguageModelToolCallPart LanguageModelPromptTsxPart LanguageModelError LanguageModelToolMode
  ChatRequestTurn ChatResponseTurn ChatResponseMarkdownPart ChatResponseAnchorPart ChatResponseProgressPart ChatResponseFileTreePart
  ChatResponseCommandButtonPart ChatResponseReferencePart ChatErrorLevel ChatVariableLevel McpHttpServerDefinition McpStdioServerDefinition
  TerminalQuickFixTerminalCommand TerminalQuickFixOpener FilePermission NotebookCellExecutionSummary DebugStackFrame DebugThread CommentReaction
  SourceControlResourceState InteractiveWindow NotebookCellStatusBarItem NotebookRendererScript NotebookDocumentContentOptions SourceBreakpoint
  FunctionBreakpoint Breakpoint DebugAdapterExecutable DebugAdapterServer DebugAdapterNamedPipeServer DebugAdapterInlineImplementation
  CustomDocumentOpenContext TerminalExitStatus TerminalLinkContext TextEditorDecorationType TextDocumentContentChangeEvent
`.split(/\s+/).filter(Boolean);

function addLenientClasses(target, names) {
  names = [...names, ...moreApiClasses];
  for (const name of names) {
    if (/^[A-Z][A-Za-z0-9]*$/.test(name) && !(name in target)) target[name] = lenientClass(name);
  }
}
