// ---- views, source control, tasks and terminals ------------------------------------------------
// Part of the extension host script (it is put in at `// @@SERVICES@@`), so everything above is
// in reach. RustCode draws these; this side keeps the objects the extension made, and names them
// to RustCode with numbers (handles) so that a click comes back as "element 7 of view x".

const wireColor = (value) => (value instanceof ThemeColor ? `theme:${value.id}` : typeof value === 'string' ? value : null);

// An argument that stands for something kept here: a tree element, a source control resource.
function resolveArgument(arg) {
  if (Array.isArray(arg)) return arg.map(resolveArgument);
  if (arg && typeof arg === 'object') {
    // A URI that went to RustCode as data comes back as one.
    if (typeof arg.scheme === 'string' && typeof arg.path === 'string' && 'fragment' in arg && !arg.$treeElement) {
      return new Uri(arg.scheme, arg.authority, arg.path, arg.query, arg.fragment);
    }
    if (arg.$treeElement) {
      const view = treeViews.get(arg.$treeElement.view);
      const entry = view && view.byHandle.get(arg.$treeElement.handle);
      return entry ? entry.element : undefined;
    }
    if (arg.$scmResource) {
      const group = scmSources.get(arg.$scmResource.source)?.groups.get(arg.$scmResource.group);
      return group && group.resourceStates[arg.$scmResource.index];
    }
    if (arg.$scmGroup) return scmSources.get(arg.$scmGroup.source)?.groups.get(arg.$scmGroup.group);
    if (arg.$scmSource) return scmSources.get(arg.$scmSource.source);
  }
  return arg;
}
const resolveArguments = (args) => (args || []).map(resolveArgument);

const viewOwner = new Map(); // view id -> extension id
const taskTypeOwner = new Map(); // task type -> extension id
const activateOwner = async (id) => {
  if (id) await activate(id);
};

const plain = (value) => JSON.parse(JSON.stringify(value ?? [], (k, v) => (v instanceof Uri ? v.toJSON() : v)));
const commandOf = (command) =>
  command && typeof command === 'object' && command.command
    ? { command: command.command, title: command.title, arguments: command.arguments || [] }
    : typeof command === 'string' ? { command, arguments: [] } : null;

const iconOf = (icon) => {
  if (!icon) return null;
  if (icon instanceof ThemeIcon) return { symbol: icon.id, color: icon.color ? icon.color.id : null };
  if (icon instanceof Uri) return { file: icon.fsPath };
  if (typeof icon === 'string') return { file: icon };
  const picked = icon.dark || icon.light;
  if (picked) return picked instanceof Uri ? { file: picked.fsPath } : { file: String(picked) };
  return null;
};

// `command` with each argument that `replace` knows a marker for replaced by it.
const withMarkers = (command, replace) => {
  if (!command) return command;
  return { ...command, arguments: plain(command.arguments.map((arg) => replace(arg) ?? arg)) };
};

const plainText = (value) => (value && typeof value === 'object' ? value.value : value);

// ---- tree views ---------------------------------------------------------------------------------

const treeViews = new Map(); // view id -> TreeViewImpl

class TreeViewImpl {
  constructor(id, options) {
    this.id = id;
    this.options = options || {};
    this.provider = this.options.treeDataProvider;
    this.byHandle = new Map(); // handle -> {element, item, parent}
    this.handleOfElement = new Map();
    this.handleOfId = new Map();
    this.nextHandle = 1;
    this.selection = [];
    this.visible = false;
    this._expanded = new EventEmitter();
    this._collapsed = new EventEmitter();
    this._selection = new EventEmitter();
    this._visibility = new EventEmitter();
    this._info = {};
    this.disposed = false;
    // Called as the provider's own method: some read `this`.
    this._subscription = this.provider && this.provider.onDidChangeTreeData ? this.provider.onDidChangeTreeData((changed) => this.changed(changed)) : undefined;
  }
  get onDidExpandElement() { return (l, t, d) => this._expanded.event(l, t, d); }
  get onDidCollapseElement() { return (l, t, d) => this._collapsed.event(l, t, d); }
  get onDidChangeSelection() { return (l, t, d) => this._selection.event(l, t, d); }
  get onDidChangeVisibility() { return (l, t, d) => this._visibility.event(l, t, d); }
  get onDidChangeCheckboxState() { return neverFires; }
  _setInfo(key, value) {
    this._info[key] = value;
    if (this._pending) return;
    this._pending = true;
    queueMicrotask(() => {
      this._pending = false;
      send({ type: 'treeInfo', view: this.id, ...this._info });
    });
  }
  get title() { return this._info.title; }
  set title(value) { this._setInfo('title', value); }
  get description() { return this._info.description; }
  set description(value) { this._setInfo('description', value); }
  get message() { return this._info.message; }
  set message(value) { this._setInfo('message', value); }
  get badge() { return this._badge; }
  set badge(value) {
    this._badge = value;
    this._setInfo('badge', value ? { value: value.value, tooltip: value.tooltip } : null);
  }
  handleFor(element, item) {
    if (item && item.id !== undefined && item.id !== null) {
      const known = this.handleOfId.get(String(item.id));
      if (known) return known;
    } else if (this.handleOfElement.has(element)) {
      return this.handleOfElement.get(element);
    }
    const handle = this.nextHandle++;
    if (item && item.id !== undefined && item.id !== null) this.handleOfId.set(String(item.id), handle);
    else this.handleOfElement.set(element, handle);
    return handle;
  }
  changed(changed) {
    const elements = Array.isArray(changed) ? changed : changed === undefined || changed === null ? [undefined] : [changed];
    for (const element of elements) {
      const handle = element === undefined ? undefined : this.handleOfElement.get(element) ?? [...this.byHandle.entries()].find(([, e]) => e.element === element)?.[0];
      send({ type: 'treeRefresh', view: this.id, handle: handle ?? null });
    }
  }
  async children(parentHandle) {
    const parent = parentHandle === null || parentHandle === undefined ? undefined : this.byHandle.get(parentHandle);
    if (parentHandle !== null && parentHandle !== undefined && !parent) return [];
    const list = (await this.provider.getChildren(parent ? parent.element : undefined)) || [];
    const nodes = [];
    for (const element of list) {
      const item = this.provider.getTreeItem ? await this.provider.getTreeItem(element) : element;
      if (!item) continue;
      const handle = this.handleFor(element, item);
      this.byHandle.set(handle, { element, item, parent: parentHandle ?? null });
      nodes.push(this.wire(handle, item, element));
    }
    return nodes;
  }
  wire(handle, item, element) {
    const label = item.label && typeof item.label === 'object' ? item.label.label : item.label;
    const named = label ?? (item.resourceUri ? path.basename(item.resourceUri.fsPath || item.resourceUri.path || '') : '');
    return {
      handle,
      label: String(named),
      description: item.description === true ? (item.resourceUri ? path.dirname(item.resourceUri.fsPath) : '') : item.description || null,
      tooltip: plainText(item.tooltip) ?? null,
      icon: iconOf(item.iconPath),
      collapsible: item.collapsibleState || 0,
      contextValue: item.contextValue ?? null,
      // An argument that is the element itself goes back as a name for it.
      command: withMarkers(commandOf(item.command), (arg) => (arg === element ? { $treeElement: { view: this.id, handle } } : undefined)),
      resource: item.resourceUri && item.resourceUri.scheme === 'file' ? item.resourceUri.fsPath : null,
    };
  }
  async reveal(element, options) {
    // Every ancestor has to be known to RustCode, which opens them from the top.
    const chain = [];
    let at = element;
    while (at !== undefined && at !== null && chain.length < 60) {
      chain.unshift(at);
      at = this.provider.getParent ? await this.provider.getParent(at) : undefined;
    }
    let parentHandle = null;
    const chainPath = [];
    for (const wanted of chain) {
      await this.children(parentHandle);
      const found = [...this.byHandle.entries()].find(([, entry]) => entry.element === wanted && entry.parent === parentHandle);
      if (!found) return;
      chainPath.push(found[0]);
      parentHandle = found[0];
    }
    send({ type: 'treeReveal', view: this.id, path: chainPath, select: options?.select !== false, expand: !!options?.expand });
  }
  event(message) {
    const elements = (message.handles || []).map((h) => this.byHandle.get(h)?.element).filter((e) => e !== undefined);
    if (message.kind === 'select') {
      this.selection = elements;
      this._selection.fire({ selection: elements });
    } else if (message.kind === 'expand') this._expanded.fire({ element: elements[0] });
    else if (message.kind === 'collapse') this._collapsed.fire({ element: elements[0] });
    else if (message.kind === 'visible') {
      this.visible = !!message.visible;
      this._visibility.fire({ visible: this.visible });
    }
  }
  dispose() {
    this.disposed = true;
    if (this._subscription) this._subscription.dispose();
    treeViews.delete(this.id);
  }
}

function createTreeView(viewId, options) {
  const view = new TreeViewImpl(viewId, options);
  treeViews.set(viewId, view);
  send({ type: 'treeReady', view: viewId });
  return view;
}

async function treeChildren({ request, view, parent }) {
  let state = treeViews.get(view);
  try {
    // The extension that adds the view starts when the view is first looked at.
    if (!state) {
      await activateOwner(viewOwner.get(view));
      state = treeViews.get(view);
    }
    const nodes = state ? await state.children(parent ?? null) : [];
    send({ type: 'treeNodes', request, view, parent: parent ?? null, nodes });
  } catch (error) {
    log(`tree view ${view}: ${describe(error)}`);
    send({ type: 'treeNodes', request, view, parent: parent ?? null, nodes: [], error: describe(error) });
  }
}

// ---- source control -----------------------------------------------------------------------------

const scmSources = new Map();

class SourceControlResourceGroup {
  constructor(source, id, label) {
    this._source = source;
    this.id = id;
    this._label = label;
    this._hideWhenEmpty = false;
    this._states = [];
    this._pending = false;
  }
  get label() { return this._label; }
  set label(value) { this._label = value; this._changed(); }
  get hideWhenEmpty() { return this._hideWhenEmpty; }
  set hideWhenEmpty(value) { this._hideWhenEmpty = !!value; this._changed(); }
  get resourceStates() { return this._states; }
  set resourceStates(value) { this._states = Array.isArray(value) ? value : []; this._changed(); }
  _changed() {
    if (this._pending || this._disposed) return;
    this._pending = true;
    queueMicrotask(() => {
      this._pending = false;
      if (!this._disposed) this._send();
    });
  }
  _send() {
    const resources = this._states.map((state, index) => ({
      index,
      path: state.resourceUri && state.resourceUri.fsPath,
      tooltip: state.decorations && state.decorations.tooltip,
      strikeThrough: !!(state.decorations && state.decorations.strikeThrough),
      faded: !!(state.decorations && state.decorations.faded),
      contextValue: state.contextValue ?? null,
      command: withMarkers(commandOf(state.command), (arg) => (arg === state ? { $scmResource: { source: this._source.id, group: this.id, index } } : undefined)),
      icon: iconOf(state.decorations && state.decorations.iconPath),
    }));
    send({ type: 'scmGroup', source: this._source.id, id: this.id, label: this._label, hideWhenEmpty: this._hideWhenEmpty, resources });
  }
  dispose() {
    this._disposed = true;
    this._source.groups.delete(this.id);
    send({ type: 'scmGroupGone', source: this._source.id, id: this.id });
  }
}

class SourceControl {
  constructor(id, label, rootUri) {
    this.id = id;
    this._label = label;
    this.rootUri = rootUri;
    this.groups = new Map();
    this._count = undefined;
    this._quickDiffProvider = undefined;
    this._commitTemplate = undefined;
    this._acceptInputCommand = undefined;
    this._statusBarCommands = [];
    this._pending = false;
    const source = this;
    const box = { _value: '', _placeholder: '', _visible: true, _enabled: true, _emitter: new EventEmitter() };
    this.inputBox = {
      get value() { return box._value; },
      set value(value) { box._value = String(value ?? ''); source._changed(); send({ type: 'scmInput', source: id, value: box._value }); },
      get placeholder() { return box._placeholder; },
      set placeholder(value) { box._placeholder = String(value ?? ''); source._changed(); },
      get visible() { return box._visible; },
      set visible(value) { box._visible = !!value; source._changed(); },
      get enabled() { return box._enabled; },
      set enabled(value) { box._enabled = !!value; },
      focus() {},
      validateInput: undefined,
      onDidChange: (l, t, d) => box._emitter.event(l, t, d),
    };
    this._box = box;
    this._changed();
  }
  get label() { return this._label; }
  set label(value) { this._label = value; this._changed(); }
  get count() { return this._count; }
  set count(value) { this._count = value; this._changed(); }
  get quickDiffProvider() { return this._quickDiffProvider; }
  set quickDiffProvider(value) { this._quickDiffProvider = value; this._changed(); }
  get commitTemplate() { return this._commitTemplate; }
  set commitTemplate(value) { this._commitTemplate = value; this._changed(); }
  get acceptInputCommand() { return this._acceptInputCommand; }
  set acceptInputCommand(value) { this._acceptInputCommand = value; this._changed(); }
  get statusBarCommands() { return this._statusBarCommands; }
  set statusBarCommands(value) { this._statusBarCommands = value || []; this._changed(); }
  _changed() {
    if (this._pending || this._disposed) return;
    this._pending = true;
    queueMicrotask(() => {
      this._pending = false;
      if (!this._disposed) this._send();
    });
  }
  _send() {
    send({
      type: 'scmSource',
      id: this.id,
      label: this._label,
      root: this.rootUri && this.rootUri.fsPath,
      count: this._count ?? null,
      inputVisible: this._box._visible,
      inputPlaceholder: this._box._placeholder,
      inputValue: this._box._value,
      accept: withMarkers(commandOf(this._acceptInputCommand), () => undefined),
      commitTemplate: this._commitTemplate ?? null,
      status: this._statusBarCommands.map((c) => withMarkers(commandOf(c), () => undefined)).filter(Boolean),
      quickDiff: !!(this.quickDiffProvider && this.quickDiffProvider.provideOriginalResource),
    });
  }
  createResourceGroup(id, label) {
    const group = new SourceControlResourceGroup(this, id, label);
    this.groups.set(id, group);
    group._changed();
    return group;
  }
  dispose() {
    this._disposed = true;
    scmSources.delete(this.id);
    send({ type: 'scmGone', id: this.id });
  }
}

const scm = {
  createSourceControl(id, label, rootUri) {
    const source = new SourceControl(id, label, rootUri);
    scmSources.set(id, source);
    return source;
  },
  get inputBox() {
    const first = scmSources.values().next().value;
    return first ? first.inputBox : { value: '', placeholder: '' };
  },
};

async function scmInput({ source, value }) {
  const found = scmSources.get(source);
  if (!found) return;
  found._box._value = String(value ?? '');
  found._box._emitter.fire(found._box._value);
  // The provider's opinion of the text, shown under the box.
  const validate = found.inputBox.validateInput;
  if (typeof validate === 'function') {
    try {
      const verdict = await validate.call(found.inputBox, found._box._value, found._box._value.length);
      send({ type: 'scmValidation', source, message: verdict ? plainText(verdict.message) : null, level: verdict ? verdict.type ?? 3 : 0 });
    } catch (error) {
      log(describe(error));
    }
  }
}

// The committed text of a file, from the provider that has one: the base of the gutter marks.
async function quickDiff({ path: filePath }) {
  for (const source of scmSources.values()) {
    const provider = source.quickDiffProvider;
    if (!provider || !provider.provideOriginalResource) continue;
    const root = source.rootUri && source.rootUri.fsPath;
    if (root && !filePath.startsWith(root)) continue;
    try {
      const original = await provider.provideOriginalResource(Uri.file(filePath), neverCancelled());
      if (!original) continue;
      const document = await openTextDocument(original);
      send({ type: 'quickDiffBase', path: filePath, text: document.getText() });
      return;
    } catch (error) {
      log(describe(error));
    }
  }
  send({ type: 'quickDiffBase', path: filePath, text: null });
}

// ---- terminals ----------------------------------------------------------------------------------

const terminalList = [];
let activeTerminal;
const terminalOpened = new EventEmitter();
const terminalClosed = new EventEmitter();
const terminalActiveChanged = new EventEmitter();
const terminalById = new Map();
let nextTerminalId = 1;

class Terminal {
  constructor(id, options, pty) {
    this._id = id;
    this.name = options.name || 'Terminal';
    this.creationOptions = Object.freeze({ ...options });
    this.exitStatus = undefined;
    this.state = { isInteractedWith: false };
    this.shellIntegration = undefined;
    this._pending = [];
    this._current = undefined;
    this._pty = pty;
    this._processId = new Promise((resolve) => (this._resolveProcess = resolve));
  }
  get processId() { return this._processId; }
  sendText(text, addNewLine = true) {
    if (this._pty) return;
    send({ type: 'terminalSend', id: this._id, text: String(text) + (addNewLine ? '\r' : '') });
  }
  show(preserveFocus) {
    send({ type: 'terminalShow', id: this._id, preserveFocus: !!preserveFocus });
  }
  hide() {
    send({ type: 'terminalHide', id: this._id });
  }
  dispose() {
    if (this.exitStatus) return;
    if (this._pty && this._pty.close) {
      try { this._pty.close(); } catch (error) { log(describe(error)); }
    }
    send({ type: 'terminalClose', id: this._id });
  }
}

function newTerminal(options, pty) {
  const id = nextTerminalId++;
  const terminal = new Terminal(id, options, pty);
  terminalById.set(id, terminal);
  terminalList.push(terminal);
  if (pty) {
    // What the extension draws goes to the terminal; what is typed goes to the extension.
    if (pty.onDidWrite) pty.onDidWrite((data) => send({ type: 'terminalWrite', id, data: String(data) }));
    if (pty.onDidClose) pty.onDidClose((code) => closeTerminal(terminal, code));
  }
  const cwd = options.cwd instanceof Uri ? options.cwd.fsPath : options.cwd;
  send({
    type: 'terminalCreate',
    id,
    name: terminal.name,
    cwd: cwd ?? null,
    env: Object.entries(options.env || {}).filter(([, v]) => typeof v === 'string'),
    shell: options.shellPath ?? null,
    shellArgs: Array.isArray(options.shellArgs) ? options.shellArgs : [],
    pseudo: !!pty,
    preserveFocus: !!options.hideFromUser,
  });
  terminalOpened.fire(terminal);
  return terminal;
}

function closeTerminal(terminal, code, quiet) {
  if (terminal.exitStatus) return;
  terminal.exitStatus = { code: code ?? undefined, reason: 1 };
  terminalById.delete(terminal._id);
  const at = terminalList.indexOf(terminal);
  if (at >= 0) terminalList.splice(at, 1);
  if (activeTerminal === terminal) {
    activeTerminal = undefined;
    terminalActiveChanged.fire(undefined);
  }
  terminalClosed.fire(terminal);
  // A tab the user opened is already gone; one an extension asked for has to be closed here.
  if (!quiet) send({ type: 'terminalClose', id: terminal._id });
}

function createTerminal(nameOrOptions, shellPath, shellArgs) {
  const options = typeof nameOrOptions === 'object' && nameOrOptions ? { ...nameOrOptions } : { name: nameOrOptions, shellPath, shellArgs };
  return newTerminal(options, options.pty);
}

// ---- shell integration ----------------------------------------------------------------------------
// A shell that prints marks about its prompt and commands (see `shell_setup.rs`) lets extensions
// run a command and read what it printed and how it ended.

const USER_TERMINAL_BASE = 4294967296;
const shellIntegrationChanged = new EventEmitter();
const shellExecutionStarted = new EventEmitter();
const shellExecutionEnded = new EventEmitter();
const TerminalShellExecutionCommandLineConfidence = { Low: 0, Medium: 1, High: 2 };

// A terminal the user opened is known to extensions as soon as RustCode first speaks of it.
function hostTerminal(id, name) {
  let terminal = terminalById.get(id);
  if (!terminal && id >= USER_TERMINAL_BASE) {
    terminal = new Terminal(id, { name: name || 'shell' }, undefined);
    terminalById.set(id, terminal);
    terminalList.push(terminal);
    terminalOpened.fire(terminal);
  }
  return terminal;
}

class TerminalShellExecution {
  constructor(commandLine, cwd, trusted) {
    this.commandLine = Object.freeze({ value: commandLine, isTrusted: !!trusted, confidence: trusted ? 2 : 0 });
    this.cwd = cwd;
    this._chunks = [];
    this._waiting = [];
    this._done = false;
  }
  _push(data) {
    this._chunks.push(data);
    this._wake();
  }
  _finish() {
    this._done = true;
    this._wake();
  }
  _wake() {
    const waiting = this._waiting;
    this._waiting = [];
    for (const resolve of waiting) resolve();
  }
  read() {
    const execution = this;
    return {
      async *[Symbol.asyncIterator]() {
        let at = 0;
        for (;;) {
          while (at < execution._chunks.length) yield execution._chunks[at++];
          if (execution._done) return;
          await new Promise((resolve) => execution._waiting.push(resolve));
        }
      },
    };
  }
}

class TerminalShellIntegration {
  constructor(terminal) {
    this._terminal = terminal;
    this.cwd = undefined;
  }
  executeCommand(commandOrExecutable, args) {
    const line = Array.isArray(args)
      ? [commandOrExecutable, ...args].map((part) => (/^[\w@%+=:,./-]+$/.test(part) ? part : `'${String(part).replace(/'/g, `'\\''`)}'`)).join(' ')
      : String(commandOrExecutable);
    const execution = new TerminalShellExecution(line, this.cwd, true);
    this._terminal._pending.push(execution);
    send({ type: 'terminalSend', id: this._terminal._id, text: line + '\r' });
    return execution;
  }
}

function terminalShell(message) {
  const terminal = hostTerminal(message.id, message.name);
  if (!terminal) return;
  const event = message.event;
  if (event === 'integrated') {
    terminal.shellIntegration = new TerminalShellIntegration(terminal);
    shellIntegrationChanged.fire({ terminal, shellIntegration: terminal.shellIntegration });
  } else if (!terminal.shellIntegration) {
    return;
  } else if (event === 'cwd') {
    terminal.shellIntegration.cwd = Uri.file(message.cwd);
    shellIntegrationChanged.fire({ terminal, shellIntegration: terminal.shellIntegration });
  } else if (event === 'commandLine') {
    // A command an extension sent comes back with the line the shell read.
    const at = terminal._pending.findIndex((e) => e.commandLine.value === message.text);
    let execution;
    if (at >= 0) execution = terminal._pending.splice(at, 1)[0];
    else execution = new TerminalShellExecution(message.text, terminal.shellIntegration.cwd, message.trusted);
    terminal._current = execution;
  } else if (event === 'started') {
    if (!terminal._current) terminal._current = new TerminalShellExecution('', terminal.shellIntegration.cwd, false);
    shellExecutionStarted.fire({ terminal, shellIntegration: terminal.shellIntegration, execution: terminal._current });
  } else if (event === 'output') {
    if (terminal._current) terminal._current._push(message.data);
  } else if (event === 'ended') {
    const execution = terminal._current;
    terminal._current = undefined;
    if (!execution) return;
    execution._finish();
    shellExecutionEnded.fire({ terminal, shellIntegration: terminal.shellIntegration, execution, exitCode: message.code ?? undefined });
  }
}

// ---- terminal links -------------------------------------------------------------------------------

const terminalLinkProviders = new Set();
const terminalLinkHandles = new Map();
let nextTerminalLinkHandle = 1;

function registerTerminalLinkProvider(provider) {
  terminalLinkProviders.add(provider);
  send({ type: 'terminalLinkProviders', count: terminalLinkProviders.size });
  return new Disposable(() => {
    terminalLinkProviders.delete(provider);
    send({ type: 'terminalLinkProviders', count: terminalLinkProviders.size });
  });
}

// RustCode asks for the links in one line of a terminal.
async function terminalLinks({ request, id, name, line }) {
  const terminal = hostTerminal(id, name) || terminalById.get(id);
  const links = [];
  if (terminal) {
    for (const provider of terminalLinkProviders) {
      try {
        const found = await provider.provideTerminalLinks({ terminal, line }, neverCancelled());
        for (const link of found || []) {
          if (!(link.length > 0) || !(link.startIndex >= 0)) continue;
          const handle = nextTerminalLinkHandle++;
          terminalLinkHandles.set(handle, { provider, link });
          links.push({ start: link.startIndex, length: link.length, tooltip: link.tooltip, handle });
        }
      } catch (error) {
        log(describe(error));
      }
    }
  }
  // Old links are of no use: a line is asked again when the mouse comes back to it.
  if (terminalLinkHandles.size > 4000) {
    for (const handle of [...terminalLinkHandles.keys()].slice(0, 2000)) terminalLinkHandles.delete(handle);
  }
  send({ type: 'terminalLinks', request, links });
}

async function terminalLinkActivate({ handle }) {
  const entry = terminalLinkHandles.get(handle);
  if (!entry || !entry.provider.handleTerminalLink) return;
  try {
    await entry.provider.handleTerminalLink(entry.link);
  } catch (error) {
    send({ type: 'message', level: 'error', text: describe(error) });
  }
}

// The user did something in a terminal RustCode drew for an extension.
function terminalMessage(message) {
  const terminal = terminalById.get(message.id);
  if (message.type === 'terminalReady') {
    if (!terminal) return;
    terminal._resolveProcess(message.pid ?? undefined);
    if (terminal._pty && terminal._pty.open) terminal._pty.open({ columns: message.columns || 80, rows: message.rows || 24 });
  } else if (message.type === 'terminalInput') {
    if (terminal) {
      terminal.state.isInteractedWith = true;
      if (terminal._pty && terminal._pty.handleInput) terminal._pty.handleInput(message.data);
    }
  } else if (message.type === 'terminalResize') {
    if (terminal && terminal._pty && terminal._pty.setDimensions) terminal._pty.setDimensions({ columns: message.columns, rows: message.rows });
  } else if (message.type === 'terminalGone') {
    if (terminal) closeTerminal(terminal, message.code, message.id >= USER_TERMINAL_BASE);
  } else if (message.type === 'terminalActive') {
    const next = message.id === null ? undefined : terminalById.get(message.id);
    if (next !== activeTerminal) {
      activeTerminal = next;
      terminalActiveChanged.fire(next);
    }
  }
}

Object.defineProperties(window, {
  terminals: { get: () => [...terminalList], enumerable: true },
  activeTerminal: { get: () => activeTerminal, enumerable: true },
});
Object.assign(window, {
  createTerminal,
  createTreeView,
  registerTreeDataProvider: (viewId, provider) => createTreeView(viewId, { treeDataProvider: provider }),
  onDidOpenTerminal: (l, t, d) => terminalOpened.event(l, t, d),
  onDidCloseTerminal: (l, t, d) => terminalClosed.event(l, t, d),
  onDidChangeActiveTerminal: (l, t, d) => terminalActiveChanged.event(l, t, d),
  onDidChangeTerminalState: neverFires,
  registerTerminalLinkProvider,
  onDidChangeTerminalShellIntegration: (l, t, d) => shellIntegrationChanged.event(l, t, d),
  onDidStartTerminalShellExecution: (l, t, d) => shellExecutionStarted.event(l, t, d),
  onDidEndTerminalShellExecution: (l, t, d) => shellExecutionEnded.event(l, t, d),
});

// ---- tasks --------------------------------------------------------------------------------------

class ShellExecution {
  constructor(commandOrLine, argsOrOptions, maybeOptions) {
    if (Array.isArray(argsOrOptions)) {
      this.command = commandOrLine;
      this.args = argsOrOptions;
      this.options = maybeOptions;
    } else {
      this.commandLine = typeof commandOrLine === 'string' ? commandOrLine : undefined;
      this.command = typeof commandOrLine === 'string' ? undefined : commandOrLine;
      this.args = [];
      this.options = argsOrOptions;
    }
  }
}
class ProcessExecution {
  constructor(process, argsOrOptions, maybeOptions) {
    this.process = process;
    this.args = Array.isArray(argsOrOptions) ? argsOrOptions : [];
    this.options = Array.isArray(argsOrOptions) ? maybeOptions : argsOrOptions;
  }
}
class CustomExecution {
  constructor(callback) {
    this.callback = callback;
  }
}
class TaskGroup {
  constructor(id, label) {
    this.id = id;
    this.label = label;
    this.isDefault = undefined;
  }
}
TaskGroup.Clean = new TaskGroup('clean', 'Clean');
TaskGroup.Build = new TaskGroup('build', 'Build');
TaskGroup.Rebuild = new TaskGroup('rebuild', 'Rebuild');
TaskGroup.Test = new TaskGroup('test', 'Test');
const TaskScope = { Global: 1, Workspace: 2 };
const TaskRevealKind = { Always: 1, Silent: 2, Never: 3 };
const TaskPanelKind = { Shared: 1, Dedicated: 2, New: 3 };
const ShellQuoting = { Escape: 1, Strong: 2, Weak: 3 };

class Task {
  constructor(definition, scopeOrName, nameOrSource, sourceOrExecution, executionOrMatchers, maybeMatchers) {
    this.definition = definition;
    // `(definition, scope, name, source, execution, matchers)`, or the old `(definition, name, source, execution, matchers)`.
    const old = typeof scopeOrName === 'string';
    this.scope = old ? undefined : scopeOrName;
    this.name = old ? scopeOrName : nameOrSource;
    this.source = old ? nameOrSource : sourceOrExecution;
    this.execution = old ? sourceOrExecution : executionOrMatchers;
    const matchers = old ? executionOrMatchers : maybeMatchers;
    this.problemMatchers = typeof matchers === 'string' ? [matchers] : matchers || [];
    this.isBackground = false;
    this.presentationOptions = {};
    this.group = undefined;
    this.detail = undefined;
    this.runOptions = {};
  }
}

const taskProviders = new Map(); // type -> provider
const taskHandles = new Map();
let nextTaskHandle = 1;
let nextExecution = 1;
const runningTasks = new Map(); // execution -> {task, terminal}
const taskStarted = new EventEmitter();
const taskEnded = new EventEmitter();

const quoteShell = (value) => {
  const text = typeof value === 'object' && value ? String(value.value) : String(value);
  if (typeof value === 'object' && value && value.quoting === ShellQuoting.Weak) return `"${text.replace(/(["\\$`])/g, '\\$1')}"`;
  if (/^[\w@%+=:,./-]+$/.test(text)) return text;
  return `'${text.replace(/'/g, `'\\''`)}'`;
};

function commandLineOf(execution) {
  if (execution instanceof ProcessExecution) return [execution.process, ...(execution.args || [])].map(quoteShell).join(' ');
  if (execution instanceof ShellExecution) {
    if (execution.commandLine !== undefined) return execution.commandLine;
    const command = execution.command;
    // A command given as text may already hold its own quoting; one given as an object is quoted.
    const first = typeof command === 'string' ? command : quoteShell(command);
    return [first, ...(execution.args || []).map(quoteShell)].join(' ');
  }
  return null;
}

function registerTaskProvider(type, provider) {
  taskProviders.set(type, provider);
  return new Disposable(() => taskProviders.delete(type));
}

async function allTasks(filter) {
  const found = [];
  for (const [type, provider] of taskProviders) {
    if (filter && filter.type && filter.type !== type) continue;
    try {
      for (const task of (await provider.provideTasks({ isCancellationRequested: false, onCancellationRequested: neverFires })) || []) found.push(task);
    } catch (error) {
      log(`task provider ${type}: ${describe(error)}`);
    }
  }
  return found;
}

async function fetchTasksFor({ request }) {
  // Extensions that offer tasks start when tasks are asked for.
  for (const owner of new Set(taskTypeOwner.values())) await activateOwner(owner).catch((error) => log(describe(error)));
  const list = [];
  for (const task of await allTasks()) {
    const handle = nextTaskHandle++;
    taskHandles.set(handle, task);
    const group = task.group;
    list.push({
      handle,
      name: String(task.name),
      source: typeof task.source === 'string' ? task.source : '',
      group: group ? group.id : null,
      defaultGroup: !!(group && group.isDefault),
      detail: task.detail ?? null,
    });
  }
  send({ type: 'tasks', request, tasks: list });
}

async function executeTask(task) {
  const execution = nextExecution++;
  const handle = { task, terminate: () => send({ type: 'taskStop', execution }) };
  runningTasks.set(execution, handle);
  const body = task.execution;
  taskStarted.fire({ execution: handle });
  if (body instanceof CustomExecution) {
    const pty = await body.callback(task.definition);
    const terminal = newTerminal({ name: String(task.name) }, pty);
    handle.terminal = terminal;
    const done = terminalClosed.event((closed) => {
      if (closed !== terminal) return;
      done.dispose();
      endTask(execution, closed.exitStatus && closed.exitStatus.code);
    });
    return handle;
  }
  const commandLine = commandLineOf(body);
  if (commandLine === null) {
    endTask(execution, 1);
    throw new Error(`task '${task.name}' has nothing to run`);
  }
  const options = body.options || {};
  const cwd = options.cwd instanceof Uri ? options.cwd.fsPath : options.cwd;
  const show = task.presentationOptions && task.presentationOptions.reveal;
  send({
    type: 'taskRun',
    execution,
    name: String(task.name),
    commandLine,
    cwd: cwd ?? null,
    env: Object.entries(options.env || {}).filter(([, v]) => typeof v === 'string'),
    reveal: show !== TaskRevealKind.Never,
    problemMatchers: task.problemMatchers || [],
  });
  return handle;
}

function endTask(execution, code) {
  const handle = runningTasks.get(execution);
  if (!handle) return;
  runningTasks.delete(execution);
  taskEnded.fire({ execution: handle });
}

async function runFetchedTask({ handle }) {
  const task = taskHandles.get(handle);
  if (task) await executeTask(task).catch((error) => send({ type: 'message', level: 'error', text: describe(error) }));
}

const tasks = {
  registerTaskProvider,
  fetchTasks: async (filter) => allTasks(filter),
  executeTask,
  get taskExecutions() {
    return [...runningTasks.values()];
  },
  onDidStartTask: (l, t, d) => taskStarted.event(l, t, d),
  onDidEndTask: (l, t, d) => taskEnded.event(l, t, d),
  onDidStartTaskProcess: neverFires,
  onDidEndTaskProcess: neverFires,
  onDidStartTaskProblemMatchers: neverFires,
  onDidEndTaskProblemMatchers: neverFires,
};

// ---- webviews and custom editors ---------------------------------------------------------------------
// RustCode shows the page in a system webview; this side keeps the objects the extension made. Pages
// are named to RustCode by number. A page sends messages with `acquireVsCodeApi().postMessage` and
// gets them from `webview.postMessage`; files of the extension reach it as `rustcode-resource:` URLs.

const customEditorOwner = new Map(); // view type -> extension id
const webviewPanels = new Map();
const customEditorProviders = new Map();
const webviewViewProviders = new Map();
const customDocuments = new Map();
const customEditHistory = new Map();
const webviewPanelSerializers = new Map();
const webviewPanelOwner = new Map(); // view type -> extension id (`onWebviewPanel:` activation events)
let nextWebviewId = 1;

const isWindows = process.platform === 'win32';
const resourceOrigin = isWindows ? 'http://rustcode-resource.localhost' : 'rustcode-resource://localhost';
const pageOrigin = isWindows ? 'http://rustcode-webview.localhost' : 'rustcode-webview://localhost';

function encodeFilePath(filePath) {
  let value = String(filePath).replace(/\\/g, '/');
  if (!value.startsWith('/')) value = '/' + value;
  return value.split('/').map((part) => encodeURIComponent(part).replace(/%3A/g, ':')).join('/');
}


// Typed arrays and buffers in messages to and from pages travel as `{$bin: type, data: base64}`.
const BINARY_TYPES = {
  ArrayBuffer, Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array,
  Float32Array, Float64Array, BigInt64Array, BigUint64Array,
};
function encodeBinary(value, depth = 0) {
  if (value === null || typeof value !== 'object' || depth > 50) return value;
  if (value instanceof ArrayBuffer) return { $bin: 'ArrayBuffer', data: Buffer.from(value).toString('base64') };
  if (ArrayBuffer.isView(value)) {
    const type = value.constructor.name === 'Buffer' || value instanceof DataView ? 'Uint8Array' : value.constructor.name;
    return { $bin: type, data: Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString('base64') };
  }
  if (typeof value.toJSON === 'function') return encodeBinary(value.toJSON(), depth + 1);
  if (Array.isArray(value)) return value.map((item) => encodeBinary(item, depth + 1));
  const out = {};
  for (const key of Object.keys(value)) out[key] = encodeBinary(value[key], depth + 1);
  return out;
}
function decodeBinary(value, depth = 0) {
  if (value === null || typeof value !== 'object' || depth > 50) return value;
  if (Array.isArray(value)) return value.map((item) => decodeBinary(item, depth + 1));
  if (typeof value.$bin === 'string' && typeof value.data === 'string' && Object.keys(value).length === 2) {
    const bytes = Buffer.from(value.data, 'base64');
    const Type = BINARY_TYPES[value.$bin];
    if (!Type) return bytes;
    const buffer = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    return Type === ArrayBuffer ? buffer : new Type(buffer);
  }
  const out = {};
  for (const key of Object.keys(value)) out[key] = decodeBinary(value[key], depth + 1);
  return out;
}

// A file as the page names it. `fsPath` stays the real path; `toString` is the address.
class WebviewResourceUri extends Uri {
  toString() {
    const query = this.query ? '?' + this.query : '';
    const fragment = this.fragment ? '#' + this.fragment : '';
    return `${resourceOrigin}${encodeFilePath(this.path)}${query}${fragment}`;
  }
}

function webviewOptionsWire(options) {
  options = options || {};
  const roots = Array.isArray(options.localResourceRoots) ? options.localResourceRoots.map((u) => (typeof u === 'string' ? u : u.fsPath)) : null;
  return { enableScripts: !!options.enableScripts, enableCommandUris: !!options.enableCommandUris, localResourceRoots: roots };
}

class WebviewImpl {
  constructor(id, options) {
    this._id = id;
    this._html = '';
    this._options = options || {};
    this._received = new EventEmitter();
    this.onDidReceiveMessage = (l, t, d) => this._received.event(l, t, d);
  }
  get html() { return this._html; }
  set html(value) {
    this._html = String(value);
    send({ type: 'webviewHtml', id: this._id, html: this._html });
  }
  get options() { return this._options; }
  set options(value) {
    this._options = value || {};
    send({ type: 'webviewOptions', id: this._id, options: webviewOptionsWire(this._options) });
  }
  get cspSource() { return `${resourceOrigin} ${pageOrigin}`; }
  asWebviewUri(uri) {
    const file = typeof uri === 'string' ? uri : uri.fsPath;
    return new WebviewResourceUri('rustcode-resource', 'localhost', file, uri.query || '', uri.fragment || '');
  }
  postMessage(message) {
    try {
      send({ type: 'webviewPost', id: this._id, message: encodeBinary(message) });
      return Promise.resolve(true);
    } catch (error) {
      return Promise.resolve(false);
    }
  }
}

const iconWire = (icon) => {
  if (!icon) return null;
  const pick = icon.dark || icon.light || icon;
  return typeof pick === 'string' ? pick : pick && pick.fsPath ? pick.fsPath : null;
};

class WebviewPanelImpl {
  constructor(id, viewType, title, column, options, panelOptions) {
    this._id = id;
    this.viewType = viewType;
    this._title = title;
    this._icon = undefined;
    this.viewColumn = column > 0 ? column : undefined;
    this.active = false;
    this.visible = false;
    this.options = panelOptions || {};
    this.webview = new WebviewImpl(id, options);
    this._disposed = new EventEmitter();
    this._viewState = new EventEmitter();
    this.onDidDispose = (l, t, d) => this._disposed.event(l, t, d);
    this.onDidChangeViewState = (l, t, d) => this._viewState.event(l, t, d);
    this._gone = false;
  }
  get title() { return this._title; }
  set title(value) {
    this._title = String(value);
    send({ type: 'webviewTitle', id: this._id, title: this._title });
  }
  get iconPath() { return this._icon; }
  set iconPath(value) {
    this._icon = value;
    send({ type: 'webviewIcon', id: this._id, icon: iconWire(value) });
  }
  reveal(column, preserveFocus) {
    send({ type: 'webviewReveal', id: this._id, column: column ?? null, preserveFocus: !!preserveFocus });
  }
  dispose() {
    if (this._gone) return;
    send({ type: 'webviewDispose', id: this._id });
    this._ended();
  }
  _ended() {
    if (this._gone) return;
    this._gone = true;
    webviewPanels.delete(this._id);
    this._disposed.fire();
  }
}

function createWebviewPanel(viewType, title, showOptions, options) {
  const id = nextWebviewId++;
  const column = typeof showOptions === 'number' ? showOptions : showOptions && showOptions.viewColumn !== undefined ? showOptions.viewColumn : -1;
  const preserveFocus = !!(showOptions && typeof showOptions === 'object' && showOptions.preserveFocus);
  const panel = new WebviewPanelImpl(id, viewType, String(title), column, options, options);
  webviewPanels.set(id, panel);
  send({ type: 'webviewCreate', id, viewType, title: panel.title, column, preserveFocus, options: webviewOptionsWire(options) });
  return panel;
}

// What RustCode says about a page.
function webviewMessage(message) {
  const panel = webviewPanels.get(message.id);
  if (!panel) return;
  if (message.type === 'webviewMessage') {
    panel.webview._received.fire(decodeBinary(message.message));
  } else if (message.type === 'webviewState') {
    const changed = panel.visible !== message.visible || panel.active !== message.active || panel.viewColumn !== (message.column || undefined);
    panel.visible = !!message.visible;
    panel.active = !!message.active;
    panel.viewColumn = message.column || undefined;
    if (changed) panel._viewState.fire({ webviewPanel: panel });
  } else if (message.type === 'webviewClosed') {
    panel._ended();
    const custom = customDocuments.get(message.id);
    if (custom) {
      customDocuments.delete(message.id);
      customEditHistory.delete(message.id);
      try { custom.document.dispose(); } catch (error) { log(describe(error)); }
    }
  }
}

function registerCustomEditorProvider(viewType, provider, options) {
  customEditorProviders.set(viewType, { provider, options: options || {} });
  return new Disposable(() => customEditorProviders.delete(viewType));
}

// The user opened a file with a custom editor: RustCode made the tab, the extension fills it.
async function customEditorOpen({ id, viewType, path: filePath }) {
  const fail = (text) => {
    send({ type: 'message', level: 'error', text });
    send({ type: 'webviewDispose', id });
  };
  try {
    if (!customEditorProviders.has(viewType)) await activateOwner(customEditorOwner.get(viewType));
    const entry = customEditorProviders.get(viewType);
    if (!entry) return fail(`no editor ${viewType}: its extension did not register it`);
    const panel = new WebviewPanelImpl(id, viewType, path.basename(filePath), -1, { enableScripts: false }, {});
    panel.visible = true;
    webviewPanels.set(id, panel);
    const uri = Uri.file(filePath);
    const token = neverCancelled();
    const { provider } = entry;
    if (typeof provider.resolveCustomTextEditor === 'function') {
      const document = await openTextDocument(uri);
      await provider.resolveCustomTextEditor(document, panel, token);
    } else {
      const document = await provider.openCustomDocument(uri, { backupId: undefined, untitledDocumentData: undefined }, token);
      customDocuments.set(id, { document, provider });
      if (provider.onDidChangeCustomDocument) {
        provider.onDidChangeCustomDocument((event) => {
          if (event.document !== document) return;
          if (event.undo && event.redo) customEditHistory.set(id, event);
          send({ type: 'customEditorDirty', id, dirty: true, undoable: !!(event.undo && event.redo) });
        });
      }
      await provider.resolveCustomEditor(document, panel, token);
    }
  } catch (error) {
    fail(`${viewType}: ${describe(error)}`);
  }
}

async function customEditorAction(message) {
  const custom = customDocuments.get(message.id);
  if (!custom) return;
  const { document, provider } = custom;
  const token = neverCancelled();
  try {
    if (message.type === 'customEditorSave') {
      if (message.target && provider.saveCustomDocumentAs) await provider.saveCustomDocumentAs(document, Uri.file(message.target), token);
      else await provider.saveCustomDocument(document, token);
      send({ type: 'customEditorSaved', id: message.id });
    } else if (message.type === 'customEditorRevert') {
      if (provider.revertCustomDocument) await provider.revertCustomDocument(document, token);
      send({ type: 'customEditorSaved', id: message.id });
    } else if (message.type === 'customEditorHistory') {
      const event = customEditHistory.get(message.id);
      if (event) await (message.redo ? event.redo() : event.undo());
    }
  } catch (error) {
    send({ type: 'message', level: 'error', text: describe(error) });
  }
}

// A view of the side bar that an extension fills with a page.
function registerWebviewViewProvider(viewId, provider, options) {
  webviewViewProviders.set(viewId, { provider, options: options || {} });
  send({ type: 'treeReady', view: viewId });
  return new Disposable(() => webviewViewProviders.delete(viewId));
}

async function webviewViewOpen({ id, view }) {
  try {
    if (!webviewViewProviders.has(view)) await activateOwner(viewOwner.get(view));
    const entry = webviewViewProviders.get(view);
    if (!entry) return send({ type: 'webviewDispose', id });
    const panel = new WebviewPanelImpl(id, view, '', -1, { enableScripts: false }, {});
    panel.visible = true;
    webviewPanels.set(id, panel);
    const hidden = { title: undefined, description: undefined, badge: undefined };
    const viewObject = {
      viewType: view,
      webview: panel.webview,
      get visible() { return panel.visible; },
      get title() { return hidden.title; },
      set title(value) { hidden.title = value; send({ type: 'webviewViewInfo', id, title: value ?? null, description: hidden.description ?? null, badge: hidden.badge?.value ?? null }); },
      get description() { return hidden.description; },
      set description(value) { hidden.description = value; send({ type: 'webviewViewInfo', id, title: hidden.title ?? null, description: value ?? null, badge: hidden.badge?.value ?? null }); },
      get badge() { return hidden.badge; },
      set badge(value) { hidden.badge = value; send({ type: 'webviewViewInfo', id, title: hidden.title ?? null, description: hidden.description ?? null, badge: value?.value ?? null }); },
      show() { send({ type: 'webviewReveal', id, column: null, preserveFocus: false }); },
      onDidChangeVisibility: (l, t, d) => panel._viewState.event(() => l.call(t), undefined, d),
      onDidDispose: panel.onDidDispose,
    };
    await entry.provider.resolveWebviewView(viewObject, { state: undefined }, neverCancelled());
  } catch (error) {
    send({ type: 'message', level: 'error', text: `${view}: ${describe(error)}` });
  }
}

function registerWebviewPanelSerializer(viewType, serializer) {
  webviewPanelSerializers.set(viewType, serializer);
  send({ type: 'webviewSerializers', viewTypes: [...webviewPanelSerializers.keys()] });
  return new Disposable(() => {
    webviewPanelSerializers.delete(viewType);
    send({ type: 'webviewSerializers', viewTypes: [...webviewPanelSerializers.keys()] });
  });
}

// A panel from the last run: RustCode made the tab, the extension fills it from the state it kept.
async function webviewRestore({ id, viewType, title, state }) {
  try {
    if (!webviewPanelSerializers.has(viewType)) await activateOwner(webviewPanelOwner.get(viewType));
    const serializer = webviewPanelSerializers.get(viewType);
    if (!serializer) return send({ type: 'webviewDispose', id });
    const panel = new WebviewPanelImpl(id, viewType, title, 1, { enableScripts: false }, {});
    panel.visible = true;
    webviewPanels.set(id, panel);
    await serializer.deserializeWebviewPanel(panel, state ?? undefined);
  } catch (error) {
    send({ type: 'message', level: 'error', text: `${viewType}: ${describe(error)}` });
    send({ type: 'webviewDispose', id });
  }
}

Object.assign(window, {
  createWebviewPanel,
  registerWebviewPanelSerializer,
  registerWebviewViewProvider,
  registerCustomEditorProvider,
});

const serviceExports = {
  scm,
  tasks,
  Task,
  ShellExecution,
  ProcessExecution,
  CustomExecution,
  TaskGroup,
  TaskScope,
  TaskRevealKind,
  TaskPanelKind,
  ShellQuoting,
  TerminalExitReason: { Unknown: 0, Shutdown: 1, Process: 2, User: 3, Extension: 4 },
  TerminalLocation: { Panel: 1, Editor: 2 },
  TerminalShellExecutionCommandLineConfidence,
};

// Messages from RustCode about these.
function serviceMessage(message) {
  switch (message.type) {
    case 'treeChildren': return void treeChildren(message);
    case 'treeEvent': return void treeViews.get(message.view)?.event(message);
    case 'scmInput': return void scmInput(message);
    case 'quickDiff': return void quickDiff(message);
    case 'fetchTasks': return void fetchTasksFor(message);
    case 'executeTask': return void runFetchedTask(message);
    case 'taskEnded': return endTask(message.execution, message.code);
    case 'webviewMessage':
    case 'webviewState':
    case 'webviewClosed':
      return webviewMessage(message);
    case 'customEditorOpen': return later(() => customEditorOpen(message));
    case 'webviewViewOpen': return void webviewViewOpen(message);
    case 'customEditorSave':
    case 'customEditorRevert':
    case 'customEditorHistory':
      return void customEditorAction(message);
    case 'webviewRestore': return void webviewRestore(message);
    case 'terminalShell': return terminalShell(message);
    case 'terminalLinks': return void terminalLinks(message);
    case 'terminalLinkActivate': return void terminalLinkActivate(message);
    case 'terminalReady':
    case 'terminalInput':
    case 'terminalResize':
    case 'terminalGone':
    case 'terminalActive':
      return terminalMessage(message);
    default: return undefined;
  }
}
