// The extension host: runs VS Code extensions against a small `vscode` module.
//
// It talks to RustCode with one JSON message per line on stdin and stdout.
//   in:  {type:"init", extensions:[{id,path,main,activationEvents,commands}], folders:[path]}
//        {type:"execute", id, command, args}
//        {type:"workbench", open:[doc], change:[doc], close:[path], active:{path,selections,tabSize,insertSpaces}|null}
//             doc = {path, languageId, version, text, dirty}; selections = [{anchor:[line,col], head:[line,col]}]
//        {type:"editResult", request, applied}   {type:"saved", path}   {type:"willSave", request, path}
//   out: {type:"message", level:"info"|"warning"|"error", text}
//        {type:"status", text}   {type:"log", text}
//        {type:"result", id, error?}
//        {type:"edit", request, changes:[change, ...]}   (answered with editResult); a change is one of
//             {op:"edit", path, version?, edits:[{line,column,endLine,endColumn,newText}]}
//             {op:"create", path, overwrite, ignoreIfExists, contents?}   {op:"delete", path, recursive, ignoreIfNotExists}
//             {op:"rename", from, to, overwrite, ignoreIfExists}
//        {type:"show", path, selections}   {type:"save", path}
//        {type:"snippet", request, path, snippet, range:{start,end}|null}   (answered with editResult)
//        {type:"prompt", request, title?, pick|input|message|openDialog|saveDialog:{...}}   (answered with promptAnswer)
//        {type:"promptInvalid", request, text}   {type:"promptDone", request}   {type:"progress", id, text, done}
//        {type:"promptAnswer", request, picked?:[index], text?, button?, paths?, cancelled?}   (in)
//        {type:"statusItem", id, text, tooltip?, command?, arguments, left, priority}   {type:"statusItemGone", id}
//        {type:"provide", request, kind, path, line, character, endLine?, endCharacter?, tabSize?, insertSpaces?}
//        {type:"providers", kinds:{kind:[{language?,scheme?,pattern?}]}}   {type:"provided", request, kind, result|error}
//        {type:"diagnostics", collection, path, items:[LSP diagnostic]}
//        {type:"willSaveListeners", count}   {type:"willSaveDone", request, edits:[{line,column,endLine,endColumn,newText}]}
//        {type:"decorations", path, key, style:{background,color,wholeLine}, ranges:[{start,end,after}]}   {type:"decorationsDrop", key}
//
// Only part of the API exists. Anything else throws "not supported by RustCode yet" when it is
// used, so an extension fails with a clear message instead of a confusing one.

'use strict';

const path = require('path');
const Module = require('module');
const readline = require('readline');

const send = (message) => process.stdout.write(JSON.stringify(message) + '\n');
const log = (text) => send({ type: 'log', text: String(text) });
const describe = (error) => {
  const text = String((error && (error.stack || error.message)) || error);
  // The error that caused it is where the real stack is.
  return error && error.cause ? `${text}\nCaused by: ${describe(error.cause)}` : text;
};

// ---- small building blocks ---------------------------------------------------------------------

class Disposable {
  constructor(callOnDispose) {
    this._callOnDispose = callOnDispose;
  }
  dispose() {
    const call = this._callOnDispose;
    this._callOnDispose = undefined;
    if (call) call();
  }
  static from(...disposables) {
    return new Disposable(() => disposables.forEach((d) => d && d.dispose && d.dispose()));
  }
}

// What each emitter's listeners are. Kept here, not in the object, so a class built on the emitter
// can have members of any name, and `fire` and `event` work when taken off the emitter.
const listenersOf = new WeakMap();
const listeners = (emitter) => {
  let list = listenersOf.get(emitter);
  if (!list) {
    list = [];
    listenersOf.set(emitter, list);
  }
  return list;
};

class EventEmitter {
  get event() {
    // The same function each time, and bound: `const { event } = emitter` is used.
    const self = this;
    return (listener, thisArg, disposables) => {
      const bound = thisArg ? listener.bind(thisArg) : listener;
      listeners(self).push(bound);
      const disposable = new Disposable(() => {
        const list = listeners(self);
        const at = list.indexOf(bound);
        if (at >= 0) list.splice(at, 1);
      });
      if (disposables) disposables.push(disposable);
      return disposable;
    };
  }
  get listenerCount() {
    return listeners(this).length;
  }
  fire(event) {
    for (const listener of [...listeners(this)]) {
      try {
        listener(event);
      } catch (error) {
        log(describe(error));
      }
    }
  }
  dispose() {
    listeners(this).length = 0;
  }
}

// Events that never fire, for the `onDid...` members of namespaces we do not model.
const neverFires = () => new Disposable(() => {});

// ---- settings --------------------------------------------------------------------------------
// RustCode gives the settings as one flat object keyed by full name (`nice.level`).

let configuration = {};
// What extensions declare as the default of each setting, apart from the values in use.
let declaredDefaults = {};

// The value of a key, or of a section: an object made from the keys under it.
// Settings of the editor itself that extensions read, with the defaults VS Code has for them.
const builtinSettings = {
  'search.exclude': { '**/node_modules': true, '**/bower_components': true, '**/*.code-search': true },
  'files.exclude': { '**/.git': true, '**/.svn': true, '**/.hg': true, '**/CVS': true, '**/.DS_Store': true, '**/Thumbs.db': true },
  'files.watcherExclude': { '**/.git/objects/**': true, '**/.git/subtree-cache/**': true, '**/node_modules/*/**': true, '**/.hg/store/**': true },
};

// `contributes.configurationDefaults` of the extensions: values for settings of the editor and
// for the `[language]` blocks, under the user's own.
const contributedDefaults = {};

function lookup(key, source = configuration) {
  if (key in source) return source[key];
  if (source === configuration && key in contributedDefaults) return contributedDefaults[key];
  if (source === configuration && key in builtinSettings) return builtinSettings[key];
  const prefix = key ? key + '.' : '';
  let found;
  for (const name of Object.keys(source)) {
    if (!name.startsWith(prefix)) continue;
    found = found || {};
    const parts = name.slice(prefix.length).split('.');
    let at = found;
    for (const part of parts.slice(0, -1)) at = at[part] = at[part] && typeof at[part] === 'object' ? at[part] : {};
    at[parts[parts.length - 1]] = source[name];
  }
  return found;
}

function configure(values, defaults) {
  if (defaults) declaredDefaults = defaults;
  const before = configuration;
  configuration = values || {};
  const changed = Object.keys({ ...before, ...configuration }).filter((k) => JSON.stringify(before[k]) !== JSON.stringify(configuration[k]));
  if (changed.length === 0) return;
  configurationChanged.fire({
    affectsConfiguration: (section) => changed.some((k) => k === section || k.startsWith(section + '.')),
  });
}

class Uri {
  constructor(scheme, authority, uriPath, query, fragment) {
    this.scheme = scheme;
    this.authority = authority || '';
    this.path = uriPath || '';
    this.query = query || '';
    this.fragment = fragment || '';
  }
  get fsPath() {
    return this.path;
  }
  static file(fsPath) {
    return new Uri('file', '', String(fsPath).replace(/\\/g, '/'), '', '');
  }
  static from(parts) {
    return new Uri(parts.scheme, parts.authority, parts.path, parts.query, parts.fragment);
  }
  static parse(value) {
    const match = /^([a-zA-Z][\w+.-]*):(?:\/\/([^/?#]*))?([^?#]*)(?:\?([^#]*))?(?:#(.*))?$/.exec(value);
    if (!match) return Uri.file(value);
    return new Uri(match[1], match[2], match[3], match[4], match[5]);
  }
  static joinPath(base, ...parts) {
    return base.with({ path: path.posix.join(base.path, ...parts) });
  }
  with(change) {
    return new Uri(
      change.scheme ?? this.scheme,
      change.authority ?? this.authority,
      change.path ?? this.path,
      change.query ?? this.query,
      change.fragment ?? this.fragment,
    );
  }
  toString() {
    const query = this.query ? '?' + this.query : '';
    const fragment = this.fragment ? '#' + this.fragment : '';
    // Like VS Code: the slashes belong to a file or a host, not to a bare path (`untitled:Name`).
    const slashes = this.authority || this.scheme === 'file' ? '//' : '';
    return `${this.scheme}:${slashes}${this.authority}${this.path}${query}${fragment}`;
  }
  toJSON() {
    return { scheme: this.scheme, authority: this.authority, path: this.path, query: this.query, fragment: this.fragment };
  }
}

class Position {
  constructor(line, character) {
    this.line = line;
    this.character = character;
  }
  isBefore(other) {
    return this.compareTo(other) < 0;
  }
  isBeforeOrEqual(other) {
    return this.compareTo(other) <= 0;
  }
  isAfter(other) {
    return this.compareTo(other) > 0;
  }
  isAfterOrEqual(other) {
    return this.compareTo(other) >= 0;
  }
  isEqual(other) {
    return this.compareTo(other) === 0;
  }
  compareTo(other) {
    return this.line - other.line || this.character - other.character;
  }
  translate(lineDelta, characterDelta) {
    if (lineDelta && typeof lineDelta === 'object') {
      return new Position(this.line + (lineDelta.lineDelta || 0), this.character + (lineDelta.characterDelta || 0));
    }
    return new Position(this.line + (lineDelta || 0), this.character + (characterDelta || 0));
  }
  with(line, character) {
    if (line && typeof line === 'object') return new Position(line.line ?? this.line, line.character ?? this.character);
    return new Position(line ?? this.line, character ?? this.character);
  }
  toJSON() {
    return { line: this.line, character: this.character };
  }
}

class Range {
  constructor(startLine, startCharacter, endLine, endCharacter) {
    let start = startLine instanceof Position ? startLine : new Position(startLine, startCharacter);
    let end = startLine instanceof Position ? startCharacter : new Position(endLine, endCharacter);
    if (start.isAfter(end)) [start, end] = [end, start];
    this.start = start;
    this.end = end;
  }
  get isEmpty() {
    return this.start.isEqual(this.end);
  }
  get isSingleLine() {
    return this.start.line === this.end.line;
  }
  contains(positionOrRange) {
    if (positionOrRange instanceof Range) return this.contains(positionOrRange.start) && this.contains(positionOrRange.end);
    return positionOrRange.isAfterOrEqual(this.start) && positionOrRange.isBeforeOrEqual(this.end);
  }
  isEqual(other) {
    return this.start.isEqual(other.start) && this.end.isEqual(other.end);
  }
  intersection(other) {
    const start = this.start.isAfter(other.start) ? this.start : other.start;
    const end = this.end.isBefore(other.end) ? this.end : other.end;
    return start.isAfter(end) ? undefined : new Range(start, end);
  }
  union(other) {
    return new Range(this.start.isBefore(other.start) ? this.start : other.start, this.end.isAfter(other.end) ? this.end : other.end);
  }
  with(start, end) {
    if (start && !(start instanceof Position) && typeof start === 'object') return new Range(start.start ?? this.start, start.end ?? this.end);
    return new Range(start ?? this.start, end ?? this.end);
  }
  toJSON() {
    return [this.start, this.end];
  }
}

class Selection extends Range {
  constructor(anchorLine, anchorCharacter, activeLine, activeCharacter) {
    const anchor = anchorLine instanceof Position ? anchorLine : new Position(anchorLine, anchorCharacter);
    const active = anchorLine instanceof Position ? anchorCharacter : new Position(activeLine, activeCharacter);
    super(anchor, active);
    this.anchor = anchor;
    this.active = active;
  }
  get isReversed() {
    return this.anchor.isAfter(this.active);
  }
}

// Snippet text built up piece by piece.
class SnippetString {
  constructor(value = '') {
    this.value = value;
    this._tabstop = 1;
  }
  static _escape(text) {
    return String(text).replace(/[$}\\]/g, '\\$&');
  }
  appendText(text) {
    this.value += String(text).replace(/[$}\\]/g, '\\$&');
    return this;
  }
  appendTabstop(number = this._tabstop++) {
    this.value += `$${number}`;
    return this;
  }
  appendPlaceholder(value, number = this._tabstop++) {
    const inner = typeof value === 'function' ? (() => { const nested = new SnippetString(); value(nested); return nested.value; })() : SnippetString._escape(value);
    this.value += `\${${number}:${inner}}`;
    return this;
  }
  appendChoice(values, number = this._tabstop++) {
    this.value += `\${${number}|${values.map((v) => String(v).replace(/[$}\\,|]/g, '\\$&')).join(',')}|}`;
    return this;
  }
  appendVariable(name, defaultValue) {
    const fallback = typeof defaultValue === 'function' ? (() => { const nested = new SnippetString(); defaultValue(nested); return nested.value; })() : defaultValue === undefined ? undefined : SnippetString._escape(defaultValue);
    this.value += fallback === undefined ? `\${${name}}` : `\${${name}:${fallback}}`;
    return this;
  }
}

class ThemeColor {
  constructor(id) {
    this.id = id;
  }
}

class TextEdit {
  constructor(range, newText) {
    this.range = range;
    this.newText = newText;
  }
  static replace(range, newText) {
    return new TextEdit(range, newText);
  }
  static insert(position, newText) {
    return new TextEdit(new Range(position, position), newText);
  }
  static delete(range) {
    return new TextEdit(range, '');
  }
  static setEndOfLine() {
    return new TextEdit(new Range(0, 0, 0, 0), '');
  }
}

// Changes to several files: text edits (grouped by file) and file operations, in the order they
// were added.
class WorkspaceEdit {
  constructor() {
    this._files = new Map(); // uri string -> {uri, edits}
    this._order = []; // text entries and file operations, as added
  }
  _for(uri) {
    const key = uri.toString();
    if (!this._files.has(key)) {
      const entry = { uri, edits: [] };
      this._files.set(key, entry);
      this._order.push(entry);
    }
    return this._files.get(key);
  }
  replace(uri, range, newText) {
    this._for(uri).edits.push(new TextEdit(range, newText));
  }
  insert(uri, position, newText) {
    this._for(uri).edits.push(TextEdit.insert(position, newText));
  }
  delete(uri, range) {
    this._for(uri).edits.push(TextEdit.delete(range));
  }
  set(uri, edits) {
    // Each entry is an edit, or `[edit, metadata]` (annotated edits, as language clients make them).
    this._for(uri).edits = edits.map((entry) => {
      const e = Array.isArray(entry) ? entry[0] : entry;
      return e instanceof TextEdit ? e : new TextEdit(e.range, e.newText ?? (e.snippet && e.snippet.value) ?? '');
    });
  }
  get(uri) {
    const entry = this._files.get(uri.toString());
    return entry ? [...entry.edits] : [];
  }
  has(uri) {
    return this._files.has(uri.toString());
  }
  get size() {
    return this._files.size + this._order.filter((c) => c.op).length;
  }
  entries() {
    return [...this._files.values()].map(({ uri, edits }) => [uri, [...edits]]);
  }
  createFile(uri, options) {
    this._order.push({ op: 'create', uri, options: options || {} });
  }
  deleteFile(uri, options) {
    this._order.push({ op: 'delete', uri, options: options || {} });
  }
  renameFile(oldUri, newUri, options) {
    this._order.push({ op: 'rename', uri: oldUri, newUri, options: options || {} });
  }
}

// An in-memory key-value store, for `globalState` and `workspaceState`.
function memento() {
  const values = new Map();
  return {
    keys: () => [...values.keys()],
    get: (key, fallback) => (values.has(key) ? values.get(key) : fallback),
    update: (key, value) => {
      if (value === undefined) values.delete(key);
      else values.set(key, value);
      return Promise.resolve();
    },
    setKeysForSync: () => {},
  };
}

// ---- documents and editors ---------------------------------------------------------------------
// RustCode tells the host about its open text files (`workbench` messages). Text is a JavaScript
// string, so lines and columns count UTF-16 units, as they do in VS Code.

// Offsets at which each line starts. Lines end at \r\n, \r or \n.
function lineStartsOf(text) {
  const starts = [0];
  const breaks = /\r\n|\r|\n/g;
  let match;
  while ((match = breaks.exec(text))) starts.push(match.index + match[0].length);
  return starts;
}

function positionIn(starts, length, offset) {
  offset = Math.max(0, Math.min(offset, length));
  let low = 0;
  let high = starts.length - 1;
  while (low < high) {
    const middle = (low + high + 1) >> 1;
    if (starts[middle] <= offset) low = middle;
    else high = middle - 1;
  }
  return new Position(low, offset - starts[low]);
}

const wordPattern = /(-?\d*\.\d\w*)|([^`~!@#%^&*()\-=+[{\]}\\|;:'",.<>/?\s]+)/g;

const languageByExtension = {
  rs: 'rust', py: 'python', js: 'javascript', mjs: 'javascript', cjs: 'javascript', jsx: 'javascriptreact', ts: 'typescript',
  tsx: 'typescriptreact', json: 'json', jsonc: 'jsonc', md: 'markdown', html: 'html', css: 'css', scss: 'scss', go: 'go',
  c: 'c', h: 'c', cpp: 'cpp', hpp: 'cpp', java: 'java', rb: 'ruby', sh: 'shellscript', yml: 'yaml', yaml: 'yaml', toml: 'toml',
};

// How RustCode names a file, and the Uri of that name: `untitled:<title>` for a file with no path,
// the file's path otherwise.
function wireUri(wire) {
  return wire.startsWith('untitled:') ? new Uri('untitled', '', wire.slice('untitled:'.length), '', '') : Uri.file(wire);
}
const wirePath = (uri) => (uri.scheme === 'untitled' ? `untitled:${uri.path}` : uri.fsPath);

class TextLine {
  constructor(lineNumber, text, breakLength) {
    this.lineNumber = lineNumber;
    this.text = text;
    this.range = new Range(lineNumber, 0, lineNumber, text.length);
    this.rangeIncludingLineBreak = new Range(lineNumber, 0, lineNumber, text.length + breakLength);
    const indent = /^\s*/.exec(text)[0].length;
    this.firstNonWhitespaceCharacterIndex = indent;
    this.isEmptyOrWhitespace = indent === text.length;
  }
}

class TextDocument {
  constructor(data) {
    this.isClosed = false;
    this.isUntitled = typeof data.path === 'string' && data.path.startsWith('untitled:');
    this._set(data);
  }
  _set({ path: filePath, languageId, version, text, dirty }) {
    // `_path` is how RustCode names the file; an untitled one is `untitled:<title>`.
    this._path = filePath;
    this._uri = wireUri(filePath);
    this.languageId = languageId;
    this.version = version;
    this._text = text;
    this.isDirty = !!dirty;
    this._starts = undefined;
  }
  get uri() {
    return this._uri;
  }
  get fileName() {
    return this.isUntitled ? this._path.slice('untitled:'.length) : this._path;
  }
  get eol() {
    return this._text.includes('\r\n') ? 2 : 1;
  }
  _lineStarts() {
    return this._starts || (this._starts = lineStartsOf(this._text));
  }
  get lineCount() {
    return this._lineStarts().length;
  }
  lineAt(lineOrPosition) {
    const line = typeof lineOrPosition === 'number' ? lineOrPosition : lineOrPosition.line;
    const starts = this._lineStarts();
    if (!Number.isInteger(line) || line < 0 || line >= starts.length) throw new Error('Illegal value for `line`');
    const end = line + 1 < starts.length ? starts[line + 1] : this._text.length;
    const raw = this._text.slice(starts[line], end);
    const text = raw.replace(/(\r\n|\r|\n)$/, '');
    return new TextLine(line, text, raw.length - text.length);
  }
  validatePosition(position) {
    const starts = this._lineStarts();
    if (position.line < 0) return new Position(0, 0);
    if (position.line >= starts.length) return this.lineAt(starts.length - 1).range.end;
    const line = this.lineAt(position.line);
    return new Position(position.line, Math.max(0, Math.min(position.character, line.text.length)));
  }
  validateRange(range) {
    return new Range(this.validatePosition(range.start), this.validatePosition(range.end));
  }
  offsetAt(position) {
    const valid = this.validatePosition(position);
    return this._lineStarts()[valid.line] + valid.character;
  }
  positionAt(offset) {
    return positionIn(this._lineStarts(), this._text.length, offset);
  }
  getText(range) {
    if (!range) return this._text;
    return this._text.slice(this.offsetAt(range.start), this.offsetAt(range.end));
  }
  getWordRangeAtPosition(position, regex) {
    const pattern = new RegExp((regex || wordPattern).source, ((regex || wordPattern).flags || '').replace(/[gy]/g, '') + 'g');
    const valid = this.validatePosition(position);
    const text = this.lineAt(valid.line).text;
    let match;
    while ((match = pattern.exec(text))) {
      if (match[0] === '') {
        pattern.lastIndex++;
        continue;
      }
      if (match.index <= valid.character && valid.character <= match.index + match[0].length) {
        return new Range(valid.line, match.index, valid.line, match.index + match[0].length);
      }
    }
    return undefined;
  }
  save() {
    return requestSave(this._path);
  }
}

const TextEditorSelectionChangeKind = { Keyboard: 1, Mouse: 2, Command: 3 };
const TextDocumentChangeReason = { Undo: 1, Redo: 2 };
const TextDocumentSaveReason = { Manual: 1, AfterDelay: 2, FocusOut: 3 };
const EndOfLine = { LF: 1, CRLF: 2 };

class TextEditor {
  constructor(document, column = 1) {
    this.document = document;
    this.viewColumn = column;
    this.options = { tabSize: 4, insertSpaces: true, cursorStyle: 1, lineNumbers: 1 };
    this._selections = [new Selection(0, 0, 0, 0)];
  }
  get selections() {
    return this._selections;
  }
  set selections(value) {
    this._selections = value && value.length ? value : [new Selection(0, 0, 0, 0)];
    send({ type: 'show', path: this.document._path, selections: wireSelections(this._selections), onlySelect: true, column: this.viewColumn });
    selectionChanged.fire({ textEditor: this, selections: this._selections, kind: TextEditorSelectionChangeKind.Command });
  }
  get selection() {
    return this._selections[0];
  }
  set selection(value) {
    this.selections = [value];
  }
  get visibleRanges() {
    return [new Range(0, 0, Math.max(0, this.document.lineCount - 1), 0)];
  }
  // Runs `callback` with a builder; all its changes are made together, as one undo step. Resolves
  // to false when the document changed since the editor was handed out.
  edit(callback) {
    const edits = [];
    const builder = {
      replace: (where, text) => edits.push(new TextEdit(where instanceof Position ? new Range(where, where) : where, text)),
      insert: (position, text) => edits.push(TextEdit.insert(position, text)),
      delete: (range) => edits.push(TextEdit.delete(range)),
      setEndOfLine() {},
    };
    callback(builder);
    return requestEdit([{ uri: this.document.uri, edits }], this.document.version);
  }
  // Puts the snippet at `location` (a Position or Range, or a list of them: it goes in each),
  // or at the primary selection. Resolves to whether it was inserted.
  insertSnippet(snippet, location) {
    const places = (Array.isArray(location) ? location : location ? [location] : []).map((place) => (place instanceof Position ? new Range(place, place) : place));
    const request = nextRequest++;
    return new Promise((resolve) => {
      waitingEdits.set(request, resolve);
      send({
        type: 'snippet',
        request,
        path: this.document._path,
        snippet: typeof snippet === 'string' ? snippet : snippet.value,
        ranges: places.map((range) => ({ start: [range.start.line, range.start.character], end: [range.end.line, range.end.character] })),
      });
    });
  }
  // Ranges of this editor's file that get the look of `type`; RustCode draws them. Each item is
  // a Range or {range, renderOptions: {after: {contentText}}}. Calling again replaces the ranges.
  setDecorations(type, items) {
    if (!type || !type.key) return;
    const ranges = (items || []).map((item) => {
      const range = item.range || item;
      const after = item.renderOptions && item.renderOptions.after && item.renderOptions.after.contentText;
      return { start: [range.start.line, range.start.character], end: [range.end.line, range.end.character], after: after ?? null };
    });
    send({ type: 'decorations', path: this.document._path, key: type.key, style: type.style, ranges });
  }
  revealRange() {}
  show() {}
  hide() {}
}

const documents = new Map(); // wire path -> TextDocument (the open ones)
let visibleEditors = new Map(); // `${wire path}\0${column}` -> TextEditor, the editors on screen
let activeEditor;
const detached = new Map(); // path -> TextDocument read from disk for openTextDocument

const documentOpened = new EventEmitter();
const documentClosed = new EventEmitter();
const documentChanged = new EventEmitter();
const documentSaved = new EventEmitter();
const activeEditorChanged = new EventEmitter();
const visibleEditorsChanged = new EventEmitter();
const selectionChanged = new EventEmitter();
const willSave = new EventEmitter();

// RustCode asks before it saves by hand only while somebody listens.
const announceWillSave = () => send({ type: 'willSaveListeners', count: willSave.listenerCount });

// Runs the `onWillSaveTextDocument` listeners and answers with the edits they asked for through
// `waitUntil`. Listeners that take longer than a second and a half are not waited for.
async function runWillSave({ request, path: filePath, reason }) {
  const document = documents.get(filePath);
  const waits = [];
  if (document) {
    willSave.fire({
      document,
      reason: reason ?? TextDocumentSaveReason.Manual,
      waitUntil: (thenable) => waits.push(Promise.resolve(thenable)),
    });
  }
  const timeout = new Promise((resolve) => setTimeout(() => resolve('timeout'), 1400));
  const settled = await Promise.race([Promise.allSettled(waits), timeout]);
  const edits = [];
  if (settled !== 'timeout') {
    for (const result of settled) {
      if (result.status !== 'fulfilled' || !Array.isArray(result.value)) continue;
      for (const e of result.value) {
        if (!e || !e.range) continue;
        edits.push({ line: e.range.start.line, column: e.range.start.character, endLine: e.range.end.line, endColumn: e.range.end.character, newText: e.newText });
      }
    }
  }
  send({ type: 'willSaveDone', request, edits });
}

const wireSelections = (selections) =>
  selections.map((s) => ({ anchor: [s.anchor.line, s.anchor.character], head: [s.active.line, s.active.character] }));
const readSelections = (list) =>
  (list || []).map((s) => new Selection(s.anchor[0], s.anchor[1], s.head[0], s.head[1]));

// What changed between two texts, as the one `contentChanges` entry that covers the difference.
function contentChange(before, after) {
  let prefix = 0;
  const shortest = Math.min(before.length, after.length);
  while (prefix < shortest && before.charCodeAt(prefix) === after.charCodeAt(prefix)) prefix++;
  let suffix = 0;
  while (suffix < shortest - prefix && before.charCodeAt(before.length - 1 - suffix) === after.charCodeAt(after.length - 1 - suffix)) suffix++;
  const starts = lineStartsOf(before);
  const range = new Range(positionIn(starts, before.length, prefix), positionIn(starts, before.length, before.length - suffix));
  return { range, rangeOffset: prefix, rangeLength: before.length - suffix - prefix, text: after.slice(prefix, after.length - suffix) };
}

// Requests to RustCode that answer later, by request number.
let nextRequest = 1;
const waitingEdits = new Map();
const waitingSaves = new Map(); // path -> [resolve]

// Sends changes to RustCode: text edits ({uri, edits}) and file operations ({op, uri, ...}), made
// in this order. Resolves to whether all of them were made. `version` is the document version the
// extension saw; the changes are refused when the document has moved on.
function requestEdit(changes, version) {
  const wire = [];
  for (const change of changes) {
    if (!change.op) {
      if (!change.edits.length) continue;
      wire.push({
        op: 'edit',
        path: wirePath(change.uri),
        version: version === undefined ? undefined : version,
        edits: change.edits.map((e) => ({
          line: e.range.start.line,
          column: e.range.start.character,
          endLine: e.range.end.line,
          endColumn: e.range.end.character,
          newText: e.newText,
        })),
      });
      continue;
    }
    const o = change.options;
    const common = { overwrite: !!o.overwrite, ignoreIfExists: !!o.ignoreIfExists };
    if (change.op === 'create') {
      const contents = o.contents === undefined ? undefined : typeof o.contents === 'string' ? o.contents : Buffer.from(o.contents).toString('utf8');
      wire.push({ op: 'create', path: change.uri.fsPath, ...common, contents });
    } else if (change.op === 'delete') {
      wire.push({ op: 'delete', path: change.uri.fsPath, recursive: !!o.recursive, ignoreIfNotExists: !!o.ignoreIfNotExists });
    } else if (change.op === 'rename') {
      wire.push({ op: 'rename', from: change.uri.fsPath, to: change.newUri.fsPath, ...common });
    }
  }
  if (!wire.length) return Promise.resolve(true);
  const request = nextRequest++;
  return new Promise((resolve) => {
    waitingEdits.set(request, resolve);
    send({ type: 'edit', request, changes: wire });
  });
}

function requestSave(filePath) {
  return new Promise((resolve) => {
    const doc = documents.get(filePath);
    if (!doc) return resolve(false);
    if (!doc.isDirty) return resolve(true);
    if (!waitingSaves.has(filePath)) waitingSaves.set(filePath, []);
    waitingSaves.get(filePath).push(resolve);
    send({ type: 'save', path: filePath });
    // A save that never finishes (a failed write) answers false.
    setTimeout(() => {
      const list = waitingSaves.get(filePath) || [];
      const at = list.indexOf(resolve);
      if (at >= 0) {
        list.splice(at, 1);
        resolve(false);
      }
    }, 5000);
  });
}

// Applies one `workbench` message. Messages are handled one at a time, in the order they came.
async function updateWorkbench(message) {
  const opened = [];
  const changed = [];
  const closed = [];
  for (const data of message.open || []) {
    const existing = documents.get(data.path);
    if (existing) {
      changed.push({ document: existing, before: existing._text });
      existing._set(data);
      continue;
    }
    const document = new TextDocument(data);
    documents.set(data.path, document);
    opened.push(document);
  }
  for (const data of message.change || []) {
    const document = documents.get(data.path);
    if (!document) {
      // Not seen before (the process started after it was opened).
      const fresh = new TextDocument(data);
      documents.set(data.path, fresh);
      opened.push(fresh);
      continue;
    }
    changed.push({ document, before: document._text });
    document._set(data);
  }
  for (const filePath of message.close || []) {
    const document = documents.get(filePath);
    if (!document) continue;
    document.isClosed = true;
    documents.delete(filePath);
    closed.push(document);
  }

  // The editors on screen, their selections, and the one with the focus.
  const beforeActive = activeEditor;
  const beforeVisible = visibleEditors;
  const selectionsMoved = [];
  if ('editors' in message) {
    const next = new Map();
    let focused;
    for (const data of message.editors || []) {
      const document = documents.get(data.path);
      if (!document) continue;
      const key = `${data.path}\0${data.column}`;
      let editor = beforeVisible.get(key);
      if (!editor || editor.document !== document) editor = new TextEditor(document, data.column);
      const selections = readSelections(data.selections);
      if (selections.length && JSON.stringify(wireSelections(selections)) !== JSON.stringify(wireSelections(editor._selections))) {
        const isNew = !beforeVisible.has(key);
        editor._selections = selections;
        if (!isNew) selectionsMoved.push(editor);
      }
      editor.options.tabSize = data.tabSize || editor.options.tabSize;
      editor.options.insertSpaces = data.insertSpaces ?? editor.options.insertSpaces;
      next.set(key, editor);
      if (data.active) focused = editor;
    }
    visibleEditors = next;
    activeEditor = focused;
  } else if (closed.length) {
    visibleEditors = new Map([...visibleEditors].filter(([, editor]) => !editor.document.isClosed));
    if (activeEditor && activeEditor.document.isClosed) activeEditor = undefined;
  }

  // Extensions that wait for a language start with the first file in it.
  for (const document of opened) {
    for (const [id, entry] of extensions) {
      if ((entry.info.activationEvents || []).includes(`onLanguage:${document.languageId}`)) {
        await activate(id).catch((error) => send({ type: 'message', level: 'error', text: `${id}: ${describe(error)}` }));
      }
    }
  }

  for (const document of opened) documentOpened.fire(document);
  for (const { document, before: text } of changed) {
    if (text === document._text) continue;
    documentChanged.fire({ document, contentChanges: [contentChange(text, document._text)], reason: undefined });
  }
  for (const document of closed) documentClosed.fire(document);
  if (beforeActive !== activeEditor) {
    activeEditorChanged.fire(activeEditor);
    for (const item of languageStatusItems) item.refresh();
  }
  const sameEditors = beforeVisible.size === visibleEditors.size && [...visibleEditors].every(([key, editor]) => beforeVisible.get(key) === editor);
  if (!sameEditors) visibleEditorsChanged.fire([...visibleEditors.values()]);
  for (const editor of selectionsMoved) {
    selectionChanged.fire({ textEditor: editor, selections: editor._selections, kind: TextEditorSelectionChangeKind.Keyboard });
  }
  // A save finishes when the document is clean again.
  for (const [filePath, waiting] of [...waitingSaves]) {
    const document = documents.get(filePath);
    if (document && !document.isDirty) {
      waitingSaves.delete(filePath);
      waiting.forEach((resolve) => resolve(true));
    }
  }
}

// Documents some extension provides the text of, by scheme (`registerTextDocumentContentProvider`).
const contentProviders = new Map();
const contentProviderChanged = new EventEmitter();
const shownVirtual = new Map(); // uri text -> the title of the untitled editor showing it

async function openTextDocument(what) {
  if (what && typeof what === 'object' && !(what instanceof Uri) && 'content' in what) {
    const document = new TextDocument({ path: `untitled:Untitled-${nextRequest++}`, languageId: what.language || 'plaintext', version: 1, text: what.content || '', dirty: true });
    return document;
  }
  const uri = what instanceof Uri ? what : typeof what === 'string' ? Uri.file(what) : what;
  if (uri.scheme === 'untitled' && documents.has(wirePath(uri))) return documents.get(wirePath(uri));
  // A scheme an extension provides the text for.
  if (uri.scheme !== 'file' && uri.scheme !== 'untitled') {
    const provider = contentProviders.get(uri.scheme);
    if (!provider) throw new Error(`Unable to open '${uri.toString()}': no content provider for the scheme '${uri.scheme}'`);
    const text = String((await provider.provideTextDocumentContent(uri, { isCancellationRequested: false })) ?? '');
    const key = uri.toString();
    const known = detached.get(key);
    if (known && known._text === text) return known;
    const document = new TextDocument({ path: key, languageId: languageByExtension[path.extname(uri.path).slice(1).toLowerCase()] || 'plaintext', version: known ? known.version + 1 : 1, text, dirty: false });
    document._uri = uri;
    document._virtual = true;
    detached.set(key, document);
    return document;
  }
  const filePath = uri.fsPath;
  if (documents.has(filePath)) return documents.get(filePath);
  const fs = require('fs');
  const text = await fs.promises.readFile(filePath, 'utf8').catch(() => {
    throw new Error(`Unable to read file '${filePath}' (file not found or not readable)`);
  });
  const known = detached.get(filePath);
  if (known && known._text === text) return known;
  const extension = path.extname(filePath).slice(1).toLowerCase();
  const document = new TextDocument({ path: filePath, languageId: languageByExtension[extension] || 'plaintext', version: known ? known.version + 1 : 1, text, dirty: false });
  detached.set(filePath, document);
  return document;
}

// The editor on screen for `document`, the focused one first.
function editorShowing(document) {
  const showing = [...visibleEditors.values()].filter((e) => e.document === document);
  return showing.find((e) => e === activeEditor) || showing[0];
}

// Resolves with the focused editor once it shows `wirePath`, or after two seconds with whatever
// there is: RustCode opens files on its own time.
function whenFocused(wire, document) {
  return new Promise((resolve) => {
    const done = (editor) => {
      subscription.dispose();
      clearTimeout(timer);
      resolve(editor || editorShowing(document) || new TextEditor(document));
    };
    const subscription = activeEditorChanged.event((editor) => {
      if (editor && editor.document._path === wire) done(editor);
    });
    const timer = setTimeout(() => done(undefined), 2000);
  });
}

async function showTextDocument(what, options) {
  const document = what instanceof TextDocument ? what : await openTextDocument(what);
  const settings = options && typeof options === 'object' ? options : {};
  const range = settings.selection;
  const selections = range ? wireSelections([new Selection(range.start, range.end)]) : [];
  if (document._virtual) {
    // Text that no file holds is shown in an untitled editor of that name.
    const title = path.posix.basename(document.uri.path) || document.uri.scheme;
    shownVirtual.set(document.uri.toString(), title);
    send({ type: 'showVirtual', title, content: document._text });
    return whenFocused(`untitled:${title}`, document);
  }
  const existing = editorShowing(document);
  if (existing) {
    // An editor that is already on screen gets the focus (also in another column).
    if (existing !== activeEditor) send({ type: 'show', path: document._path, selections, column: existing.viewColumn });
    else if (range) send({ type: 'show', path: document._path, selections, onlySelect: true, column: existing.viewColumn });
    if (range) existing._selections = [new Selection(range.start, range.end)];
    return existing;
  }
  // Not on screen: RustCode opens it, then it shows up in a `workbench` message.
  send({ type: 'show', path: document._path, selections });
  return whenFocused(document._path, document);
}

// ---- languages: diagnostics and providers ------------------------------------------------------
// Extensions register providers (`languages.registerHoverProvider(...)`) and report problems
// (`createDiagnosticCollection`). RustCode asks a provider with a `provide` message and gets the
// answer as LSP-shaped JSON, which it already knows how to read.

const DiagnosticSeverity = { Error: 0, Warning: 1, Information: 2, Hint: 3 };
const DiagnosticTag = { Unnecessary: 1, Deprecated: 2 };

class Location {
  constructor(uri, rangeOrPosition) {
    this.uri = uri;
    this.range = rangeOrPosition instanceof Position ? new Range(rangeOrPosition, rangeOrPosition) : rangeOrPosition;
  }
}

class DiagnosticRelatedInformation {
  constructor(location, message) {
    this.location = location;
    this.message = message;
  }
}

class Diagnostic {
  constructor(range, message, severity = DiagnosticSeverity.Error) {
    this.range = range;
    this.message = message;
    this.severity = severity;
  }
}

class MarkdownString {
  constructor(value = '', supportThemeIcons = false) {
    this.value = value;
    this.supportThemeIcons = supportThemeIcons;
    this.isTrusted = false;
    this.supportHtml = false;
  }
  appendText(text) {
    this.value += String(text).replace(/[\\`*_{}[\]()#+\-.!|<>]/g, '\\$&').replace(/\n/g, '  \n');
    return this;
  }
  appendMarkdown(text) {
    this.value += text;
    return this;
  }
  appendCodeblock(code, language = '') {
    this.value += `\n\`\`\`${language}\n${code}\n\`\`\`\n`;
    return this;
  }
}

class Hover {
  constructor(contents, range) {
    this.contents = Array.isArray(contents) ? contents : [contents];
    this.range = range;
  }
}

const CompletionItemKind = {
  Text: 0, Method: 1, Function: 2, Constructor: 3, Field: 4, Variable: 5, Class: 6, Interface: 7, Module: 8, Property: 9, Unit: 10,
  Value: 11, Enum: 12, Keyword: 13, Snippet: 14, Color: 15, File: 16, Reference: 17, Folder: 18, EnumMember: 19, Constant: 20,
  Struct: 21, Event: 22, Operator: 23, TypeParameter: 24, User: 25, Issue: 26,
};
const CompletionTriggerKind = { Invoke: 0, TriggerCharacter: 1, TriggerForIncompleteCompletions: 2 };

class CompletionItem {
  constructor(label, kind) {
    this.label = label;
    this.kind = kind;
  }
}

class CompletionList {
  constructor(items = [], isIncomplete = false) {
    this.items = items;
    this.isIncomplete = isIncomplete;
  }
}

class CodeActionKind {
  constructor(value) {
    this.value = value;
  }
  append(part) {
    return new CodeActionKind(this.value ? `${this.value}.${part}` : part);
  }
  contains(other) {
    return this.value === other.value || other.value.startsWith(this.value + '.') || this.value === '';
  }
  intersects(other) {
    return this.contains(other) || other.contains(this);
  }
}
CodeActionKind.Empty = new CodeActionKind('');
CodeActionKind.QuickFix = new CodeActionKind('quickfix');
CodeActionKind.Refactor = new CodeActionKind('refactor');
CodeActionKind.RefactorExtract = new CodeActionKind('refactor.extract');
CodeActionKind.RefactorInline = new CodeActionKind('refactor.inline');
CodeActionKind.RefactorRewrite = new CodeActionKind('refactor.rewrite');
CodeActionKind.Source = new CodeActionKind('source');
CodeActionKind.SourceOrganizeImports = new CodeActionKind('source.organizeImports');
CodeActionKind.SourceFixAll = new CodeActionKind('source.fixAll');
const CodeActionTriggerKind = { Invoke: 1, Automatic: 2 };

class CodeAction {
  constructor(title, kind) {
    this.title = title;
    this.kind = kind;
  }
}

class ParameterInformation {
  constructor(label, documentation) {
    this.label = label;
    this.documentation = documentation;
  }
}
class SignatureInformation {
  constructor(label, documentation) {
    this.label = label;
    this.documentation = documentation;
    this.parameters = [];
  }
}
class SignatureHelp {
  constructor() {
    this.signatures = [];
    this.activeSignature = 0;
    this.activeParameter = 0;
  }
}
const SignatureHelpTriggerKind = { Invoke: 1, TriggerCharacter: 2, ContentChange: 3 };

const InlineCompletionTriggerKind = { Invoke: 0, Automatic: 1 };
class InlineCompletionItem {
  constructor(insertText, range, command) {
    this.insertText = insertText;
    this.range = range;
    this.command = command;
  }
}
class InlineCompletionList {
  constructor(items) {
    this.items = items;
  }
}

class CancellationTokenSource {
  constructor() {
    this._emitter = new EventEmitter();
    this.token = { isCancellationRequested: false, onCancellationRequested: this._emitter.event };
  }
  cancel() {
    this.token.isCancellationRequested = true;
    this._emitter.fire();
  }
  dispose() {
    this._emitter.dispose();
  }
}
// What providers are given: a request that is never cancelled.
const neverCancelled = () => ({ isCancellationRequested: false, onCancellationRequested: neverFires });

// A glob (`**/*.js`, `*.{ts,tsx}`) as a regular expression.
function globToRegExp(pattern) {
  let out = '';
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    if (c === '*') {
      if (pattern[i + 1] === '*') {
        i++;
        if (pattern[i + 1] === '/') {
          i++;
          out += '(?:.*/)?';
        } else {
          out += '.*';
        }
      } else {
        out += '[^/]*';
      }
    } else if (c === '?') out += '[^/]';
    else if (c === '{') out += '(?:';
    else if (c === '}') out += ')';
    else if (c === ',' && /\{[^}]*$/.test(pattern.slice(0, i))) out += '|';
    else if (c === '[') {
      const end = pattern.indexOf(']', i);
      if (end < 0) out += '\\[';
      else {
        out += pattern.slice(i, end + 1).replace('[!', '[^');
        i = end;
      }
    } else out += c.replace(/[.+^$()|\\]/g, '\\$&');
  }
  return new RegExp(`^${out}$`);
}

function globMatches(pattern, filePath) {
  const text = typeof pattern === 'string' ? pattern : pattern.pattern;
  const normalized = String(filePath).replace(/\\/g, '/');
  // A pattern with no slash is about the file's name.
  if (!text.includes('/')) return globToRegExp(text).test(normalized.slice(normalized.lastIndexOf('/') + 1));
  const anchored = text.startsWith('/') || text.startsWith('**') ? text : `**/${text}`;
  return globToRegExp(anchored).test(normalized);
}

// How well a selector fits a document: 0 when it does not.
function selectorScore(selector, document) {
  if (Array.isArray(selector)) return Math.max(0, ...selector.map((s) => selectorScore(s, document)));
  if (typeof selector === 'string') return selector === '*' ? 5 : selector === document.languageId ? 10 : 0;
  if (!selector) return 0;
  let present = 0;
  if (selector.language) {
    present++;
    if (selector.language !== '*' && selector.language !== document.languageId) return 0;
  }
  if (selector.scheme) {
    present++;
    if (selector.scheme !== '*' && selector.scheme !== document.uri.scheme) return 0;
  }
  if (selector.pattern) {
    present++;
    if (!globMatches(selector.pattern, document.fileName)) return 0;
  }
  return present ? 10 : 0;
}

// A selector as plain data for RustCode, which decides whether to ask at all.
function selectorFilters(selector) {
  const list = Array.isArray(selector) ? selector : [selector];
  return list.map((s) =>
    typeof s === 'string'
      ? { language: s === '*' ? undefined : s }
      : { language: s.language === '*' ? undefined : s.language, scheme: s.scheme, pattern: s.pattern && (typeof s.pattern === 'string' ? s.pattern : s.pattern.pattern) },
  );
}

const providers = []; // {id, kind, selector, provider, extra}
let nextProviderId = 1;

function announceProviders() {
  const kinds = {};
  const triggers = {};
  for (const entry of providers) {
    (kinds[entry.kind] = kinds[entry.kind] || []).push(...selectorFilters(entry.selector));
    if (entry.extra && entry.extra.triggers) (triggers[entry.kind] = triggers[entry.kind] || []).push(...entry.extra.triggers);
  }
  send({ type: 'providers', kinds, triggers });
}

function registerProvider(kind, selector, provider, extra) {
  const entry = { id: nextProviderId++, kind, selector, provider, extra };
  providers.push(entry);
  announceProviders();
  return new Disposable(() => {
    const at = providers.indexOf(entry);
    if (at >= 0) providers.splice(at, 1);
    announceProviders();
  });
}

// Kinds of provider RustCode has nothing to show for (code lenses, highlights, colors...) are
// taken, so that an extension that registers them still loads; nobody asks them.
const registerNothing = () => new Disposable(() => {});

// ---- turning what providers give into what RustCode reads (LSP shapes)

const wireRange = (range) => ({ start: { line: range.start.line, character: range.start.character }, end: { line: range.end.line, character: range.end.character } });

function markdownOf(content) {
  if (content === undefined || content === null) return '';
  if (typeof content === 'string') return content;
  if (typeof content.value === 'string') return content.language ? `\`\`\`${content.language}\n${content.value}\n\`\`\`` : content.value;
  return '';
}

function wireDiagnostic(d) {
  return {
    range: wireRange(d.range),
    message: d.message,
    severity: (d.severity ?? 0) + 1,
    source: d.source,
    code: d.code && typeof d.code === 'object' ? d.code.value : d.code,
    tags: d.tags,
  };
}

function wireWorkspaceEdit(edit) {
  const changes = {};
  for (const [uri, edits] of edit.entries()) {
    changes[uri.toString()] = edits.map((e) => ({ range: wireRange(e.range), newText: e.newText }));
  }
  return { changes };
}

// The suggestions last given, by number, so that one can be asked for its rest.
const givenCompletions = new Map();
let nextCompletionId = 1;

function wireCompletion(item, provider) {
  const label = typeof item.label === 'string' ? item.label : item.label && item.label.label;
  const id = nextCompletionId++;
  givenCompletions.set(id, { item, provider });
  if (givenCompletions.size > 3000) givenCompletions.delete(givenCompletions.keys().next().value);
  const out = { label, kind: item.kind === undefined ? undefined : item.kind + 1, detail: item.detail, sortText: item.sortText, filterText: item.filterText, preselect: item.preselect, rustcodeItem: id };
  if (item.label && typeof item.label === 'object' && item.label.detail) out.detail = out.detail ? out.detail : item.label.detail;
  if (item.documentation) out.documentation = { kind: 'markdown', value: markdownOf(item.documentation) };
  const insert = item.insertText;
  let text = label;
  if (insert instanceof SnippetString) {
    text = insert.value;
    out.insertTextFormat = 2;
  } else if (typeof insert === 'string') text = insert;
  out.insertText = text;
  const range = item.range && item.range.replacing ? item.range.replacing : item.range;
  if (range instanceof Range) out.textEdit = { range: wireRange(range), newText: text };
  if (item.additionalTextEdits) out.additionalTextEdits = item.additionalTextEdits.map((e) => ({ range: wireRange(e.range), newText: e.newText }));
  return out;
}

function wireLocation(l) {
  if (l.targetUri) {
    return { targetUri: l.targetUri.toString(), targetRange: wireRange(l.targetRange), targetSelectionRange: wireRange(l.targetSelectionRange || l.targetRange) };
  }
  return { uri: l.uri.toString(), range: wireRange(l.range) };
}

function wireCodeAction(item) {
  // A Command is offered as an action that runs it.
  const isCommand = typeof item.command === 'string';
  const command = isCommand ? item : item.command;
  const out = isCommand ? { title: item.title } : { title: item.title, kind: item.kind && item.kind.value, isPreferred: item.isPreferred, diagnostics: item.diagnostics && item.diagnostics.map(wireDiagnostic) };
  if (!isCommand && item.edit) out.edit = wireWorkspaceEdit(item.edit);
  if (command) {
    out.command = { title: command.title || item.title, command: command.command, arguments: command.arguments };
    // RustCode runs this one in the extension host, not on a language server.
    out.rustcodeExtensionCommand = { command: command.command, arguments: command.arguments || [] };
  }
  return out;
}

function wireSignature(help) {
  return {
    signatures: help.signatures.map((s) => ({
      label: s.label,
      documentation: s.documentation && { kind: 'markdown', value: markdownOf(s.documentation) },
      parameters: (s.parameters || []).map((p) => ({ label: p.label, documentation: p.documentation && { kind: 'markdown', value: markdownOf(p.documentation) } })),
    })),
    activeSignature: help.activeSignature,
    activeParameter: help.activeParameter,
  };
}

const toRange = (m) => new Range(m.line, m.character, m.endLine ?? m.line, m.endCharacter ?? m.character);

// What each kind of question asks the providers, and how it turns the answers into one. `token`
// says when the question is no longer wanted.
const wireSymbolKind = (kind) => (kind ?? 0) + 1;
const wireSymbol = (symbol) =>
  symbol.location
    ? { name: symbol.name, kind: wireSymbolKind(symbol.kind), location: wireLocation(symbol.location), containerName: symbol.containerName }
    : {
        name: symbol.name,
        detail: symbol.detail,
        kind: wireSymbolKind(symbol.kind),
        range: wireRange(symbol.range),
        selectionRange: wireRange(symbol.selectionRange || symbol.range),
        children: (symbol.children || []).map(wireSymbol),
      };

const provideHandlers = {
  async hover(entries, document, m, token) {
    const parts = [];
    for (const { provider } of entries) {
      const hover = await provider.provideHover(document, new Position(m.line, m.character), token);
      if (hover) parts.push(...(Array.isArray(hover.contents) ? hover.contents : [hover.contents]).map(markdownOf).filter(Boolean));
    }
    return parts.length ? { contents: { kind: 'markdown', value: parts.join('\n\n') } } : null;
  },
  async completion(entries, document, m, token) {
    const items = [];
    for (const { provider } of entries) {
      const found = await provider.provideCompletionItems(document, new Position(m.line, m.character), token, { triggerKind: CompletionTriggerKind.Invoke, triggerCharacter: undefined });
      const list = Array.isArray(found) ? found : found && found.items;
      if (list) items.push(...list.map((item) => wireCompletion(item, provider)));
    }
    return items;
  },
  // The rest of a suggestion this host gave: what its provider adds when asked.
  async resolveCompletion(entries, document, m, token) {
    const given = givenCompletions.get(m.id);
    if (!given) return null;
    const resolved = typeof given.provider.resolveCompletionItem === 'function' ? (await given.provider.resolveCompletionItem(given.item, token)) || given.item : given.item;
    const wire = wireCompletion(resolved, given.provider);
    return { documentation: wire.documentation, detail: wire.detail, additionalTextEdits: wire.additionalTextEdits };
  },
  definition: (entries, document, m, token) => places(entries, 'provideDefinition', document, m, token),
  typeDefinition: (entries, document, m, token) => places(entries, 'provideTypeDefinition', document, m, token),
  implementation: (entries, document, m, token) => places(entries, 'provideImplementation', document, m, token),
  async references(entries, document, m, token) {
    const found = [];
    for (const { provider } of entries) {
      const list = await provider.provideReferences(document, new Position(m.line, m.character), { includeDeclaration: true }, token);
      if (list) found.push(...list.map(wireLocation));
    }
    return found;
  },
  async format(entries, document, m, token) {
    for (const { provider } of entries) {
      const edits = await provider.provideDocumentFormattingEdits(document, { tabSize: m.tabSize, insertSpaces: m.insertSpaces }, token);
      if (edits && edits.length) return edits.map((e) => ({ range: wireRange(e.range), newText: e.newText }));
    }
    return [];
  },
  async formatRange(entries, document, m, token) {
    for (const { provider } of entries) {
      const edits = await provider.provideDocumentRangeFormattingEdits(document, toRange(m), { tabSize: m.tabSize, insertSpaces: m.insertSpaces }, token);
      if (edits && edits.length) return edits.map((e) => ({ range: wireRange(e.range), newText: e.newText }));
    }
    return [];
  },
  async onTypeFormat(entries, document, m, token) {
    for (const { provider } of entries) {
      const edits = await provider.provideOnTypeFormattingEdits(document, new Position(m.line, m.character), m.ch, { tabSize: m.tabSize, insertSpaces: m.insertSpaces }, token);
      if (edits && edits.length) return edits.map((e) => ({ range: wireRange(e.range), newText: e.newText }));
    }
    return [];
  },
  async codeActions(entries, document, m, token) {
    const range = toRange(m);
    const diagnostics = diagnosticsOverlapping(document.uri, range);
    const actions = [];
    for (const { provider } of entries) {
      const found = await provider.provideCodeActions(document, range, { diagnostics, only: undefined, triggerKind: CodeActionTriggerKind.Invoke }, token);
      for (const action of found || []) {
        // An action that comes without its changes is asked for them now.
        const lazy = !(typeof action.command === 'string') && !action.edit && typeof provider.resolveCodeAction === 'function';
        actions.push(wireCodeAction(lazy ? (await provider.resolveCodeAction(action, token)) || action : action));
      }
    }
    return actions;
  },
  async signature(entries, document, m, token) {
    for (const { provider } of entries) {
      const help = await provider.provideSignatureHelp(document, new Position(m.line, m.character), token, { triggerKind: SignatureHelpTriggerKind.Invoke, isRetrigger: false });
      if (help && help.signatures && help.signatures.length) return wireSignature(help);
    }
    return null;
  },
  async documentSymbols(entries, document, m, token) {
    for (const { provider } of entries) {
      const found = await provider.provideDocumentSymbols(document, token);
      if (found && found.length) return found.map(wireSymbol);
    }
    return [];
  },
  async workspaceSymbols(entries, document, m, token) {
    const found = [];
    for (const { provider } of entries) {
      const list = await provider.provideWorkspaceSymbols(m.query || '', token);
      if (list) found.push(...list.map(wireSymbol));
    }
    return found;
  },
  async rename(entries, document, m, token) {
    for (const { provider } of entries) {
      const edit = await provider.provideRenameEdits(document, new Position(m.line, m.character), m.newName, token);
      if (edit) return wireWorkspaceEdit(edit);
    }
    return null;
  },
  async foldingRanges(entries, document, m, token) {
    const found = [];
    for (const { provider } of entries) {
      const list = await provider.provideFoldingRanges(document, {}, token);
      if (list) found.push(...list.map((r) => ({ startLine: r.start, endLine: r.end, kind: { 1: 'comment', 2: 'imports', 3: 'region' }[r.kind] })));
    }
    return found;
  },
  async inlayHints(entries, document, m, token) {
    const found = [];
    const range = new Range(m.line, 0, m.endLine, 0);
    for (const { provider } of entries) {
      const list = await provider.provideInlayHints(document, range, token);
      for (const hint of list || []) {
        const label = typeof hint.label === 'string' ? hint.label : (hint.label || []).map((part) => part.value).join('');
        found.push({ position: { line: hint.position.line, character: hint.position.character }, label, kind: hint.kind, paddingLeft: !!hint.paddingLeft, paddingRight: !!hint.paddingRight });
      }
    }
    return found;
  },
  async documentLinks(entries, document, m, token) {
    const found = [];
    for (const { provider } of entries) {
      const list = await provider.provideDocumentLinks(document, token);
      if (list) found.push(...list.map((l) => ({ range: wireRange(l.range), target: l.target && l.target.toString() })));
    }
    return found;
  },
  async semanticTokens(entries, document, m, token) {
    for (const { provider, extra } of entries) {
      // A provider of ranges is asked for the whole file.
      const tokens = typeof provider.provideDocumentSemanticTokens === 'function'
        ? await provider.provideDocumentSemanticTokens(document, token)
        : await provider.provideDocumentRangeSemanticTokens(document, new Range(0, 0, document.lineCount, 0), token);
      if (tokens && tokens.data) return { data: Array.from(tokens.data), legend: { tokenTypes: extra.legend.tokenTypes, tokenModifiers: extra.legend.tokenModifiers } };
    }
    return null;
  },
  // Who calls the function at the place, or what it calls: the provider names the function first.
  async callHierarchy(entries, document, m, token) {
    const wireItem = (item) => ({
      name: item.name, kind: wireSymbolKind(item.kind), detail: item.detail, uri: item.uri.toString(),
      range: wireRange(item.range), selectionRange: wireRange(item.selectionRange || item.range),
    });
    const found = [];
    for (const { provider } of entries) {
      const prepared = await provider.prepareCallHierarchy(document, new Position(m.line, m.character), token);
      const item = Array.isArray(prepared) ? prepared[0] : prepared;
      if (!item) continue;
      const calls = m.incoming ? await provider.provideCallHierarchyIncomingCalls(item, token) : await provider.provideCallHierarchyOutgoingCalls(item, token);
      for (const call of calls || []) found.push({ [m.incoming ? 'from' : 'to']: wireItem(m.incoming ? call.from : call.to), fromRanges: (call.fromRanges || []).map(wireRange) });
    }
    return found;
  },
  // The ranges around the place, nested, as the parents of the first one the provider gives.
  async selectionRanges(entries, document, m, token) {
    const wireNested = (node) => ({ range: wireRange(node.range), parent: node.parent ? wireNested(node.parent) : undefined });
    for (const { provider } of entries) {
      const found = await provider.provideSelectionRanges(document, [new Position(m.line, m.character)], token);
      if (found && found[0]) return [wireNested(found[0])];
    }
    return [];
  },
  // The types above or below the one at the place: the provider names the type first.
  async typeHierarchy(entries, document, m, token) {
    const wireItem = (item) => ({
      name: item.name, kind: wireSymbolKind(item.kind), detail: item.detail, uri: item.uri.toString(),
      range: wireRange(item.range), selectionRange: wireRange(item.selectionRange || item.range),
    });
    const found = [];
    for (const { provider } of entries) {
      const prepared = await provider.prepareTypeHierarchy(document, new Position(m.line, m.character), token);
      const item = Array.isArray(prepared) ? prepared[0] : prepared;
      if (!item) continue;
      const types = m.supertypes ? await provider.provideTypeHierarchySupertypes(item, token) : await provider.provideTypeHierarchySubtypes(item, token);
      found.push(...(types || []).map(wireItem));
    }
    return found;
  },
  // What to suggest typing at the caret, offered dimly until it is taken.
  async inlineCompletions(entries, document, m, token) {
    const found = [];
    for (const { provider } of entries) {
      const list = await provider.provideInlineCompletionItems(document, new Position(m.line, m.character), { triggerKind: InlineCompletionTriggerKind.Automatic, selectedCompletionInfo: undefined }, token);
      for (const item of Array.isArray(list) ? list : (list && list.items) || []) {
        // A snippet is marked as one, so that its tab stops are not typed.
        const insertText = typeof item.insertText === 'string' ? item.insertText : item.insertText && { kind: 'snippet', value: item.insertText.value };
        if (insertText) found.push({ insertText, range: item.range && wireRange(item.range) });
      }
    }
    return found;
  },
  // Lenses come with their commands: one that has none is resolved here.
  async codeLenses(entries, document, m, token) {
    const found = [];
    for (const { provider } of entries) {
      for (let lens of (await provider.provideCodeLenses(document, token)) || []) {
        if (!lens.command && typeof provider.resolveCodeLens === 'function') lens = (await provider.resolveCodeLens(lens, token)) || lens;
        if (!lens.command) continue;
        found.push({
          range: wireRange(lens.range),
          command: { title: lens.command.title, command: lens.command.command, arguments: lens.command.arguments },
          rustcodeExtensionCommand: { command: lens.command.command, arguments: lens.command.arguments || [] },
        });
      }
    }
    return found;
  },
  // The colors written in the file; each as red, green, blue and alpha from 0 to 1.
  async documentColors(entries, document, m, token) {
    const found = [];
    for (const { provider } of entries) {
      for (const info of (await provider.provideDocumentColors(document, token)) || []) {
        const c = info.color;
        found.push({ range: wireRange(info.range), color: { red: c.red, green: c.green, blue: c.blue, alpha: c.alpha } });
      }
    }
    return found;
  },
  // The ways to write a color (bytes in `m.color`) in place of the one at the range.
  async colorPresentations(entries, document, m, token) {
    const [r, g, b, a] = m.color;
    const color = new Color(r / 255, g / 255, b / 255, a / 255);
    const found = [];
    for (const { provider } of entries) {
      const list = await provider.provideColorPresentations(color, { document, range: toRange(m) }, token);
      for (const p of list || []) {
        found.push({
          label: p.label,
          textEdit: p.textEdit && { range: wireRange(p.textEdit.range), newText: p.textEdit.newText },
        });
      }
    }
    return found;
  },
  async documentHighlights(entries, document, m, token) {
    const found = [];
    for (const { provider } of entries) {
      const list = await provider.provideDocumentHighlights(document, new Position(m.line, m.character), token);
      if (list) found.push(...list.map((h) => ({ range: wireRange(h.range), kind: (h.kind ?? 0) + 1 })));
    }
    return found;
  },
  async linkedEditing(entries, document, m, token) {
    for (const { provider } of entries) {
      const found = await provider.provideLinkedEditingRanges(document, new Position(m.line, m.character), token);
      if (found && found.ranges) return { ranges: found.ranges.map(wireRange), wordPattern: found.wordPattern && found.wordPattern.source };
    }
    return null;
  },
};

async function places(entries, method, document, m, token) {
  const found = [];
  for (const { provider } of entries) {
    const result = await provider[method](document, new Position(m.line, m.character), token);
    if (result) found.push(...(Array.isArray(result) ? result : [result]).map(wireLocation));
  }
  return found;
}

// The method a provider of each kind has, to tell which providers can answer a question.
const providerMethod = {
  hover: 'provideHover',
  completion: 'provideCompletionItems',
  resolveCompletion: 'resolveCompletionItem',
  definition: 'provideDefinition',
  typeDefinition: 'provideTypeDefinition',
  implementation: 'provideImplementation',
  references: 'provideReferences',
  format: 'provideDocumentFormattingEdits',
  formatRange: 'provideDocumentRangeFormattingEdits',
  onTypeFormat: 'provideOnTypeFormattingEdits',
  codeActions: 'provideCodeActions',
  signature: 'provideSignatureHelp',
  documentSymbols: 'provideDocumentSymbols',
  workspaceSymbols: 'provideWorkspaceSymbols',
  rename: 'provideRenameEdits',
  foldingRanges: 'provideFoldingRanges',
  inlayHints: 'provideInlayHints',
  documentLinks: 'provideDocumentLinks',
  semanticTokens: 'provideDocumentSemanticTokens',
  linkedEditing: 'provideLinkedEditingRanges',
  callHierarchy: 'prepareCallHierarchy',
  typeHierarchy: 'prepareTypeHierarchy',
  selectionRanges: 'provideSelectionRanges',
  inlineCompletions: 'provideInlineCompletionItems',
  documentHighlights: 'provideDocumentHighlights',
  documentColors: 'provideDocumentColors',
  colorPresentations: 'provideColorPresentations',
  codeLenses: 'provideCodeLenses',
};

// The question being answered for each kind and file, so a newer one can call the older off.
const runningProvides = new Map();

// Answers one `provide` message. It does not hold up the messages behind it. An older question
// of the same kind about the same file is called off (its token says so) when a newer one comes.
async function runProvide(message) {
  const { request, kind, path: wire } = message;
  const document = documents.get(wire) || (kind === 'workspaceSymbols' ? [...documents.values()][0] : undefined);
  const wanted = kind === 'resolveCompletion' ? 'completion' : kind === 'colorPresentations' ? 'documentColors' : kind;
  const entries = providers.filter((p) => p.kind === wanted && (kind === 'workspaceSymbols' || (document && selectorScore(p.selector, document) > 0)));
  if (!document || !entries.length || !provideHandlers[kind]) {
    send({ type: 'provided', request, kind, result: null });
    return;
  }
  const key = `${kind}\0${wire}`;
  runningProvides.get(key)?.cancel();
  const source = new CancellationTokenSource();
  runningProvides.set(key, source);
  try {
    const result = await provideHandlers[kind](entries, document, message, source.token);
    send({ type: 'provided', request, kind, result: source.token.isCancellationRequested ? null : result });
  } catch (error) {
    send({ type: 'provided', request, kind, error: describe(error) });
  } finally {
    if (runningProvides.get(key) === source) runningProvides.delete(key);
    source.dispose();
  }
}

// ---- diagnostics

const diagnosticCollections = new Set();
const diagnosticsChanged = new EventEmitter();

class DiagnosticCollection {
  constructor(name) {
    this.name = name;
    this._map = new Map(); // wire path -> {uri, diagnostics}
    diagnosticCollections.add(this);
  }
  _put(uri, diagnostics) {
    const key = wirePath(uri);
    const list = diagnostics ? [...diagnostics] : [];
    if (list.length) this._map.set(key, { uri, diagnostics: list });
    else this._map.delete(key);
    send({ type: 'diagnostics', collection: this.name, path: key, items: list.map(wireDiagnostic) });
    diagnosticsChanged.fire({ uris: [uri] });
  }
  set(uriOrEntries, diagnostics) {
    if (Array.isArray(uriOrEntries)) {
      for (const [uri, list] of uriOrEntries) this._put(uri, list);
    } else this._put(uriOrEntries, diagnostics);
  }
  delete(uri) {
    this._put(uri, undefined);
  }
  clear() {
    for (const { uri } of [...this._map.values()]) this._put(uri, undefined);
  }
  forEach(callback, thisArg) {
    for (const { uri, diagnostics } of [...this._map.values()]) callback.call(thisArg, uri, diagnostics, this);
  }
  get(uri) {
    const entry = this._map.get(wirePath(uri));
    return entry ? [...entry.diagnostics] : [];
  }
  has(uri) {
    return this._map.has(wirePath(uri));
  }
  dispose() {
    this.clear();
    diagnosticCollections.delete(this);
  }
  [Symbol.iterator]() {
    return [...this._map.values()].map(({ uri, diagnostics }) => [uri, diagnostics])[Symbol.iterator]();
  }
}

// What every collection holds for a file that touches `range`, for code action requests.
function diagnosticsOverlapping(uri, range) {
  const found = [];
  for (const collection of diagnosticCollections) {
    for (const d of collection.get(uri)) if (d.range.intersection(range) || d.range.contains(range)) found.push(d);
  }
  return found;
}

// A note about the language of the file in focus (`languages.createLanguageStatusItem`): drawn in
// the status bar while the focused editor's file matches the selector.
const languageStatusItems = new Set();
class LanguageStatusItem {
  constructor(id, selector) {
    this.id = id;
    this.selector = selector;
    this._item = new StatusBarItem(nextRequest++, 2, -500);
    this.name = undefined;
    this.severity = 0;
    this.busy = false;
    this.accessibilityInformation = undefined;
    this._text = '';
    this._detail = undefined;
    this._command = undefined;
    languageStatusItems.add(this);
    this.refresh();
  }
  get text() {
    return this._text;
  }
  set text(value) {
    this._text = String(value ?? '');
    this._item.text = this._text;
  }
  get detail() {
    return this._detail;
  }
  set detail(value) {
    this._detail = value;
    this._item.tooltip = value;
  }
  get command() {
    return this._command;
  }
  set command(value) {
    this._command = value;
    this._item.command = value;
  }
  // Shown while the focused editor's file is one this item is about.
  refresh() {
    const editor = activeEditor;
    if (editor && selectorScore(this.selector, editor.document) > 0) this._item.show();
    else this._item.hide();
  }
  dispose() {
    languageStatusItems.delete(this);
    this._item.dispose();
  }
}

const knownLanguages = ['plaintext', ...new Set(Object.values(languageByExtension))];

const languages = {
  createDiagnosticCollection: (name) => new DiagnosticCollection(name || `collection-${nextRequest++}`),
  createLanguageStatusItem: (id, selector) => new LanguageStatusItem(id, selector),
  getDiagnostics(uri) {
    if (uri) return [...diagnosticCollections].flatMap((c) => c.get(uri));
    const byFile = new Map();
    for (const collection of diagnosticCollections) {
      for (const [file, list] of collection) {
        const key = file.toString();
        if (!byFile.has(key)) byFile.set(key, [file, []]);
        byFile.get(key)[1].push(...list);
      }
    }
    return [...byFile.values()];
  },
  onDidChangeDiagnostics: (listener, thisArg, disposables) => diagnosticsChanged.event(listener, thisArg, disposables),
  getLanguages: () => Promise.resolve([...new Set([...knownLanguages, ...[...documents.values()].map((d) => d.languageId)])]),
  match: (selector, document) => selectorScore(selector, document),
  registerHoverProvider: (selector, provider) => registerProvider('hover', selector, provider),
  registerCompletionItemProvider: (selector, provider, ...triggers) => registerProvider('completion', selector, provider, { triggers: triggers.filter((t) => typeof t === 'string') }),
  registerDefinitionProvider: (selector, provider) => registerProvider('definition', selector, provider),
  registerTypeDefinitionProvider: (selector, provider) => registerProvider('typeDefinition', selector, provider),
  registerImplementationProvider: (selector, provider) => registerProvider('implementation', selector, provider),
  registerReferenceProvider: (selector, provider) => registerProvider('references', selector, provider),
  registerDocumentFormattingEditProvider: (selector, provider) => registerProvider('format', selector, provider),
  registerDocumentRangeFormattingEditProvider: (selector, provider) => registerProvider('formatRange', selector, provider),
  registerOnTypeFormattingEditProvider: (selector, provider, first, ...more) => registerProvider('onTypeFormat', selector, provider, { triggers: [first, ...more].filter(Boolean) }),
  registerCodeActionsProvider: (selector, provider) => registerProvider('codeActions', selector, provider),
  registerSignatureHelpProvider: (selector, provider, ...triggers) =>
    registerProvider('signature', selector, provider, { triggers: triggers.flatMap((t) => (typeof t === 'string' ? [t] : [...(t.triggerCharacters || []), ...(t.retriggerCharacters || [])])) }),
  registerDocumentSymbolProvider: (selector, provider) => registerProvider('documentSymbols', selector, provider),
  registerWorkspaceSymbolProvider: (provider) => registerProvider('workspaceSymbols', '*', provider),
  registerRenameProvider: (selector, provider) => registerProvider('rename', selector, provider),
  registerFoldingRangeProvider: (selector, provider) => registerProvider('foldingRanges', selector, provider),
  registerInlayHintsProvider: (selector, provider) => registerProvider('inlayHints', selector, provider),
  registerDocumentLinkProvider: (selector, provider) => registerProvider('documentLinks', selector, provider),
  registerDocumentSemanticTokensProvider: (selector, provider, legend) => registerProvider('semanticTokens', selector, provider, { legend }),
  registerLinkedEditingRangeProvider: (selector, provider) => registerProvider('linkedEditing', selector, provider),
  // Accepted, and not asked: RustCode has nowhere to show these.
  registerCodeLensProvider: (selector, provider) => registerProvider('codeLenses', selector, provider),
  registerDocumentHighlightProvider: (selector, provider) => registerProvider('documentHighlights', selector, provider),
  registerSelectionRangeProvider: (selector, provider) => registerProvider('selectionRanges', selector, provider),
  registerColorProvider: (selector, provider) => registerProvider('documentColors', selector, provider),
  registerDocumentRangeSemanticTokensProvider: (selector, provider, legend) => registerProvider('semanticTokens', selector, provider, { legend }),
  registerCallHierarchyProvider: (selector, provider) => registerProvider('callHierarchy', selector, provider),
  registerTypeHierarchyProvider: (selector, provider) => registerProvider('typeHierarchy', selector, provider),
  registerEvaluatableExpressionProvider: registerNothing,
  registerInlineValuesProvider: registerNothing,
  registerInlineCompletionItemProvider: (selector, provider) => registerProvider('inlineCompletions', selector, provider),
  registerDeclarationProvider: registerNothing,
  registerDocumentDropEditProvider: registerNothing,
  registerDocumentPasteEditProvider: registerNothing,
};

// ---- more of the API's data classes and enums --------------------------------------------------

class CodeLens {
  constructor(range, command) {
    this.range = range;
    this.command = command;
  }
  get isResolved() {
    return !!this.command;
  }
}
class DocumentLink {
  constructor(range, target) {
    this.range = range;
    this.target = target;
  }
}
const SymbolKind = {
  File: 0, Module: 1, Namespace: 2, Package: 3, Class: 4, Method: 5, Property: 6, Field: 7, Constructor: 8, Enum: 9, Interface: 10,
  Function: 11, Variable: 12, Constant: 13, String: 14, Number: 15, Boolean: 16, Array: 17, Object: 18, Key: 19, Null: 20,
  EnumMember: 21, Struct: 22, Event: 23, Operator: 24, TypeParameter: 25,
};
const SymbolTag = { Deprecated: 1 };
class SymbolInformation {
  constructor(name, kind, rangeOrContainer, locationOrUri, containerName) {
    this.name = name;
    this.kind = kind;
    if (typeof rangeOrContainer === 'string' || rangeOrContainer === undefined) {
      this.containerName = rangeOrContainer;
      this.location = locationOrUri;
    } else {
      this.containerName = containerName;
      this.location = new Location(locationOrUri, rangeOrContainer);
    }
  }
}
class DocumentSymbol {
  constructor(name, detail, kind, range, selectionRange) {
    this.name = name;
    this.detail = detail;
    this.kind = kind;
    this.range = range;
    this.selectionRange = selectionRange;
    this.children = [];
  }
}
const FoldingRangeKind = { Comment: 1, Imports: 2, Region: 3 };
class FoldingRange {
  constructor(start, end, kind) {
    this.start = start;
    this.end = end;
    this.kind = kind;
  }
}
class SelectionRange {
  constructor(range, parent) {
    this.range = range;
    this.parent = parent;
  }
}
const InlayHintKind = { Type: 1, Parameter: 2 };
class InlayHintLabelPart {
  constructor(value) {
    this.value = value;
  }
}
class InlayHint {
  constructor(position, label, kind) {
    this.position = position;
    this.label = label;
    this.kind = kind;
  }
}
const DocumentHighlightKind = { Text: 0, Read: 1, Write: 2 };
class TypeHierarchyItem {
  constructor(kind, name, detail, uri, range, selectionRange) {
    Object.assign(this, { kind, name, detail, uri, range, selectionRange });
  }
}
class CallHierarchyItem {
  constructor(kind, name, detail, uri, range, selectionRange) {
    Object.assign(this, { kind, name, detail, uri, range, selectionRange });
  }
}
class CallHierarchyIncomingCall {
  constructor(item, fromRanges) {
    this.from = item;
    this.fromRanges = fromRanges;
  }
}
class CallHierarchyOutgoingCall {
  constructor(item, fromRanges) {
    this.to = item;
    this.fromRanges = fromRanges;
  }
}
class DocumentHighlight {
  constructor(range, kind = DocumentHighlightKind.Text) {
    this.range = range;
    this.kind = kind;
  }
}
class Color {
  constructor(red, green, blue, alpha) {
    Object.assign(this, { red, green, blue, alpha });
  }
}
class ColorInformation {
  constructor(range, color) {
    this.range = range;
    this.color = color;
  }
}
class ColorPresentation {
  constructor(label) {
    this.label = label;
  }
}
class SemanticTokensLegend {
  constructor(tokenTypes, tokenModifiers = []) {
    this.tokenTypes = tokenTypes;
    this.tokenModifiers = tokenModifiers;
  }
}
class SemanticTokens {
  constructor(data, resultId) {
    this.data = data;
    this.resultId = resultId;
  }
}
class SemanticTokensEdit {
  constructor(start, deleteCount, data) {
    Object.assign(this, { start, deleteCount, data });
  }
}
class SemanticTokensEdits {
  constructor(edits, resultId) {
    this.edits = edits;
    this.resultId = resultId;
  }
}
// Collects tokens in any order and gives them as the packed numbers LSP uses.
class SemanticTokensBuilder {
  constructor(legend) {
    this._legend = legend;
    this._tokens = [];
  }
  push(lineOrRange, charOrLength, lengthOrType, typeOrModifiers, maybeModifiers) {
    const ranged = lineOrRange instanceof Range;
    const [line, char, length, type, modifiers] = ranged
      ? [lineOrRange.start.line, lineOrRange.start.character, lineOrRange.end.character - lineOrRange.start.character, charOrLength, lengthOrType]
      : [lineOrRange, charOrLength, lengthOrType, typeOrModifiers, maybeModifiers];
    const typeIndex = typeof type === 'string' && this._legend ? Math.max(0, this._legend.tokenTypes.indexOf(type)) : type;
    const bits = Array.isArray(modifiers) && this._legend ? modifiers.reduce((acc, m) => acc | (1 << Math.max(0, this._legend.tokenModifiers.indexOf(m))), 0) : modifiers || 0;
    this._tokens.push([line, char, length, typeIndex, bits]);
  }
  build(resultId) {
    const sorted = [...this._tokens].sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    const data = [];
    let previousLine = 0;
    let previousChar = 0;
    for (const [line, char, length, type, bits] of sorted) {
      const deltaLine = line - previousLine;
      data.push(deltaLine, deltaLine === 0 ? char - previousChar : char, length, type, bits);
      previousLine = line;
      previousChar = char;
    }
    return new SemanticTokens(Uint32Array.from(data), resultId);
  }
}
class ThemeIcon {
  constructor(id, color) {
    this.id = id;
    this.color = color;
  }
}
ThemeIcon.File = new ThemeIcon('file');
ThemeIcon.Folder = new ThemeIcon('folder');
const TreeItemCollapsibleState = { None: 0, Collapsed: 1, Expanded: 2 };
class TreeItem {
  constructor(labelOrUri, collapsibleState = 0) {
    if (typeof labelOrUri === 'string' || (labelOrUri && typeof labelOrUri === 'object' && 'label' in labelOrUri)) this.label = labelOrUri;
    else this.resourceUri = labelOrUri;
    this.collapsibleState = collapsibleState;
  }
}
class CancellationError extends Error {
  constructor() {
    super('Canceled');
    this.name = 'Canceled';
  }
}
class FileSystemError extends Error {
  constructor(messageOrUri) {
    super(typeof messageOrUri === 'string' ? messageOrUri : String(messageOrUri));
    this.code = 'Unknown';
  }
  static FileNotFound(m) { return Object.assign(new FileSystemError(m), { code: 'FileNotFound' }); }
  static FileExists(m) { return Object.assign(new FileSystemError(m), { code: 'FileExists' }); }
  static FileNotADirectory(m) { return Object.assign(new FileSystemError(m), { code: 'FileNotADirectory' }); }
  static FileIsADirectory(m) { return Object.assign(new FileSystemError(m), { code: 'FileIsADirectory' }); }
  static NoPermissions(m) { return Object.assign(new FileSystemError(m), { code: 'NoPermissions' }); }
  static Unavailable(m) { return Object.assign(new FileSystemError(m), { code: 'Unavailable' }); }
}
const miscEnums = {
  ProgressLocation: { SourceControl: 1, Window: 10, Notification: 15 },
  QuickPickItemKind: { Separator: -1, Default: 0 },
  FileChangeType: { Changed: 1, Created: 2, Deleted: 3 },
  LogLevel: { Off: 0, Trace: 1, Debug: 2, Info: 3, Warning: 4, Error: 5 },
  ExtensionKind: { UI: 1, Workspace: 2 },
  UIKind: { Desktop: 1, Web: 2 },
  OverviewRulerLane: { Left: 1, Center: 2, Right: 4, Full: 7 },
  DecorationRangeBehavior: { OpenOpen: 0, ClosedClosed: 1, OpenClosed: 2, ClosedOpen: 3 },
  ColorThemeKind: { Light: 1, Dark: 2, HighContrast: 3, HighContrastLight: 4 },
  CompletionItemTag: { Deprecated: 1 },
  InputBoxValidationSeverity: { Info: 1, Warning: 2, Error: 3 },
  TextEditorCursorStyle: { Line: 1, Block: 2, Underline: 3, LineThin: 4, BlockOutline: 5, UnderlineThin: 6 },
  TextEditorLineNumbersStyle: { Off: 0, On: 1, Relative: 2 },
  TreeItemCheckboxState: { Unchecked: 0, Checked: 1 },
  LanguageStatusSeverity: { Information: 0, Warning: 1, Error: 2 },
  EnvironmentVariableMutatorType: { Replace: 1, Append: 2, Prepend: 3 },
  ConfigurationTarget: { Global: 1, Workspace: 2, WorkspaceFolder: 3 },
};

// A name of the API that RustCode does not model. A capitalized one is a class, so that an
// extension can extend it at load time; it fails when it is used.
function unsupportedMember(name) {
  const last = name.slice(name.lastIndexOf('.') + 1);
  // What an extension registers for a feature RustCode does not have is accepted, and not used.
  if (/^register[A-Z]/.test(last)) return () => new Disposable(() => {});
  if (/^[A-Z]/.test(last)) {
    const stub = class {
      constructor() {
        throw new Error(`${name} is not supported by RustCode yet`);
      }
    };
    Object.defineProperty(stub, 'name', { value: name });
    return stub;
  }
  // An event that nothing here raises is one nobody hears: extensions subscribe and wait.
  if (/^on(Did|Will)[A-Z]/.test(name.slice(name.lastIndexOf('.') + 1))) return neverFires;
  return () => {
    throw new Error(`${name} is not supported by RustCode yet`);
  };
}

// A log channel (`createOutputChannel(name, {log: true})`): the usual channel, with levels.
function createLogChannel(name) {
  const line = (level, text, args) => log(`[${name}] ${level ? `[${level}] ` : ''}${[text, ...args].map(String).join(' ')}`);
  const levelChanged = new EventEmitter();
  return {
    name,
    logLevel: miscEnums.LogLevel.Info,
    onDidChangeLogLevel: levelChanged.event,
    trace: (m, ...a) => line('trace', m, a),
    debug: (m, ...a) => line('debug', m, a),
    info: (m, ...a) => line('info', m, a),
    warn: (m, ...a) => line('warn', m, a),
    error: (m, ...a) => line('error', m instanceof Error ? m.stack || m.message : m, a),
    append: (text) => line('', text, []),
    appendLine: (text) => line('', text, []),
    replace: (text) => line('', text, []),
    clear() {},
    show() {},
    hide() {},
    dispose() {},
  };
}

// File watchers (`workspace.createFileSystemWatcher`) over the workspace folders. One recursive
// watch per folder serves all the watchers; each keeps the changes its pattern names.
const fileWatchers = new Set();
const folderWatches = new Map(); // folder path -> fs.FSWatcher
function watchFolders() {
  const fs = require('fs');
  for (const folder of workspaceFolders) {
    const root = folder.uri.fsPath;
    if (folderWatches.has(root)) continue;
    try {
      const watch = fs.watch(root, { recursive: true }, (event, name) => {
        if (!name) return;
        const full = path.join(root, name);
        if (/(^|[\\/])(node_modules|\.git)([\\/]|$)/.test(name)) return;
        fs.promises.stat(full).then(
          () => dispatchFileChange(full, event === 'rename' ? 'created' : 'changed'),
          () => dispatchFileChange(full, 'deleted'),
        );
      });
      watch.on('error', () => {});
      folderWatches.set(root, watch);
    } catch (error) {
      log(`cannot watch ${root}: ${error.message}`);
    }
  }
}
function dispatchFileChange(full, kind) {
  for (const watcher of fileWatchers) {
    if (watcher._ignore[kind]) continue;
    if (!globMatches(watcher._pattern, full)) continue;
    watcher._emitters[kind].fire(Uri.file(full));
  }
}
function createFileSystemWatcher(pattern, ignoreCreate = false, ignoreChange = false, ignoreDelete = false) {
  const emitters = { created: new EventEmitter(), changed: new EventEmitter(), deleted: new EventEmitter() };
  const watcher = {
    _pattern: typeof pattern === 'string' ? pattern : pattern.pattern ? (pattern.base ? `${pattern.base.fsPath || pattern.base}/${pattern.pattern}` : pattern.pattern) : '**',
    _ignore: { created: ignoreCreate, changed: ignoreChange, deleted: ignoreDelete },
    _emitters: emitters,
    ignoreCreateEvents: ignoreCreate,
    ignoreChangeEvents: ignoreChange,
    ignoreDeleteEvents: ignoreDelete,
    onDidCreate: emitters.created.event,
    onDidChange: emitters.changed.event,
    onDidDelete: emitters.deleted.event,
    dispose() {
      fileWatchers.delete(watcher);
      Object.values(emitters).forEach((e) => e.dispose());
    },
  };
  fileWatchers.add(watcher);
  watchFolders();
  return watcher;
}

class RelativePattern {
  constructor(base, pattern) {
    this.baseUri = typeof base === 'string' ? Uri.file(base) : base.uri || base;
    this.base = this.baseUri.fsPath;
    this.pattern = pattern;
  }
}

// ---- the `vscode` module -----------------------------------------------------------------------

const handlers = new Map(); // command id -> function
let workspaceFolders = [];

// ---- asking the user ----------------------------------------------------------------------------
// A question goes to RustCode as a `prompt` message and waits for the `promptAnswer`.

const waitingPrompts = new Map(); // request -> {resolve, validate}

function prompt(fields, validate) {
  const request = nextRequest++;
  return new Promise((resolve) => {
    waitingPrompts.set(request, { resolve, validate });
    send({ type: 'prompt', request, ...fields });
  });
}

// The answer arrives. An input the extension refuses (`validateInput`) is sent back to be fixed.
async function settlePrompt(message) {
  const waiting = waitingPrompts.get(message.request);
  if (!waiting) return;
  if (waiting.validate && typeof message.text === 'string') {
    let problem;
    try {
      const found = await waiting.validate(message.text);
      problem = found && typeof found === 'object' ? found.message : found;
    } catch (error) {
      problem = String(error && error.message || error);
    }
    if (problem) {
      send({ type: 'promptInvalid', request: message.request, text: String(problem) });
      return;
    }
  }
  waitingPrompts.delete(message.request);
  waiting.resolve(message);
  // An input box stays up until its text is taken.
  if (typeof message.text === 'string') send({ type: 'promptDone', request: message.request });
}

const asPickItem = (item) => (typeof item === 'string' ? { label: item } : item);

async function showQuickPick(itemsOrThenable, options, token) {
  const given = await itemsOrThenable;
  const items = given.map(asPickItem).filter((item) => item.kind !== -1);
  const answer = await prompt({
    title: options && options.title,
    pick: {
      items: items.map((i) => ({ label: String(i.label), description: i.description, detail: i.detail, picked: !!i.picked })),
      many: !!(options && options.canPickMany),
      placeholder: options && options.placeHolder,
      matchDescription: !!(options && options.matchOnDescription),
      matchDetail: !!(options && options.matchOnDetail),
    },
  });
  if (answer.cancelled || !Array.isArray(answer.picked)) return undefined;
  const chosen = answer.picked.map((index) => items[index]).filter(Boolean);
  if (options && options.canPickMany) {
    if (options.onDidSelectItem) chosen.forEach((item) => options.onDidSelectItem(item));
    return chosen;
  }
  if (chosen[0] && options && options.onDidSelectItem) options.onDidSelectItem(chosen[0]);
  // A list that was given as strings answers with the string.
  return chosen[0] === undefined ? undefined : typeof given[items.indexOf(chosen[0])] === 'string' ? chosen[0].label : chosen[0];
}

async function showInputBox(options) {
  const o = options || {};
  const answer = await prompt(
    { title: o.title, input: { prompt: o.prompt, placeholder: o.placeHolder, value: o.value ?? '', password: !!o.password } },
    o.validateInput,
  );
  return answer.cancelled || typeof answer.text !== 'string' ? undefined : answer.text;
}

function showMessage(level) {
  return async (message, ...rest) => {
    // The first of the rest is options (`{modal, detail}`) when it is an object with no `title`.
    const options = rest.length && rest[0] && typeof rest[0] === 'object' && !('title' in rest[0]) ? rest.shift() : {};
    const items = rest.filter((item) => item !== undefined && item !== null);
    if (!items.length && !options.modal) {
      send({ type: 'message', level, text: String(message) });
      return undefined;
    }
    const titles = items.map((item) => (typeof item === 'string' ? item : String(item.title)));
    const answer = await prompt({ message: { level, text: String(message), detail: options.detail, modal: !!options.modal, buttons: titles } });
    return answer.cancelled || typeof answer.button !== 'number' ? undefined : items[answer.button];
  };
}

async function showOpenDialog(options) {
  const o = options || {};
  const answer = await prompt({
    title: o.title,
    openDialog: { files: o.canSelectFiles ?? true, folders: !!o.canSelectFolders, many: !!o.canSelectMany, default: o.defaultUri && o.defaultUri.fsPath },
  });
  return answer.cancelled || !Array.isArray(answer.paths) ? undefined : answer.paths.map((p) => Uri.file(p));
}

async function showSaveDialog(options) {
  const o = options || {};
  const answer = await prompt({ title: o.title, saveDialog: { default: o.defaultUri && o.defaultUri.fsPath } });
  return answer.cancelled || !Array.isArray(answer.paths) || !answer.paths.length ? undefined : Uri.file(answer.paths[0]);
}

// `createQuickPick()` and `createInputBox()`: the same questions, asked by `show()`. What is typed is
// not reported while it is typed, and items cannot change while the list is up.
class QuickInput {
  constructor() {
    this.title = undefined;
    this.placeholder = undefined;
    this.enabled = true;
    this.busy = false;
    this.ignoreFocusOut = false;
    this._accept = new EventEmitter();
    this._hide = new EventEmitter();
    this.onDidAccept = this._accept.event;
    this.onDidHide = this._hide.event;
    this.buttons = [];
    this.onDidTriggerButton = neverFires;
    this._open = false;
  }
  hide() {
    this._open = false;
    this._hide.fire();
  }
  dispose() {
    this._accept.dispose();
    this._hide.dispose();
  }
}

class QuickPick extends QuickInput {
  constructor() {
    super();
    this.items = [];
    this.canSelectMany = false;
    this.matchOnDescription = false;
    this.matchOnDetail = false;
    this.value = '';
    this.activeItems = [];
    this.selectedItems = [];
    this._change = new EventEmitter();
    this.onDidChangeSelection = this._change.event;
    this.onDidChangeValue = neverFires;
    this.onDidChangeActive = neverFires;
  }
  async show() {
    this._open = true;
    const picked = await showQuickPick(this.items, {
      title: this.title,
      placeHolder: this.placeholder,
      canPickMany: this.canSelectMany,
      matchOnDescription: this.matchOnDescription,
      matchOnDetail: this.matchOnDetail,
    });
    if (!this._open) return;
    if (picked !== undefined) {
      this.selectedItems = Array.isArray(picked) ? picked : [picked];
      this.activeItems = this.selectedItems;
      this._change.fire(this.selectedItems);
      this._accept.fire();
    }
    this.hide();
  }
}

class InputBox extends QuickInput {
  constructor() {
    super();
    this.value = '';
    this.prompt = undefined;
    this.password = false;
    this.validationMessage = undefined;
    this.onDidChangeValue = neverFires;
  }
  async show() {
    this._open = true;
    const text = await showInputBox({ title: this.title, prompt: this.prompt, placeHolder: this.placeholder, value: this.value, password: this.password });
    if (!this._open) return;
    if (text !== undefined) {
      this.value = text;
      this._accept.fire();
    }
    this.hide();
  }
}

const commands = {
  registerCommand(id, callback, thisArg) {
    handlers.set(id, thisArg ? callback.bind(thisArg) : callback);
    return new Disposable(() => handlers.delete(id));
  },
  // The callback gets the focused editor and an edit builder, then the command's arguments.
  registerTextEditorCommand(id, callback, thisArg) {
    return commands.registerCommand(id, (...args) => {
      const editor = activeEditor;
      if (!editor) return undefined;
      const edits = [];
      const builder = {
        replace: (where, text) => edits.push(new TextEdit(where instanceof Position ? new Range(where, where) : where, text)),
        insert: (position, text) => edits.push(TextEdit.insert(position, text)),
        delete: (range) => edits.push(TextEdit.delete(range)),
        setEndOfLine() {},
      };
      const result = callback.call(thisArg, editor, builder, ...args);
      return Promise.resolve(result).then(async (value) => {
        if (edits.length) await requestEdit([{ uri: editor.document.uri, edits }], editor.document.version);
        return value;
      });
    });
  },
  async executeCommand(id, ...args) {
    const handler = handlers.get(id);
    if (!handler) {
      // A command of the editor itself goes to RustCode.
      if (forwardEditorCommand(id, args)) return undefined;
      throw new Error(`command '${id}' not found`);
    }
    return handler(...args);
  },
  getCommands: () => Promise.resolve([...handlers.keys()]),
};

const window = {
  showQuickPick,
  showInputBox,
  showOpenDialog,
  showSaveDialog,
  createQuickPick: () => new QuickPick(),
  createInputBox: () => new InputBox(),
  showInformationMessage: showMessage('info'),
  showWarningMessage: showMessage('warning'),
  showErrorMessage: showMessage('error'),
  createOutputChannel(name, options) {
    if (options && typeof options === 'object' && options.log) return createLogChannel(name);
    const line = (text) => log(`[${name}] ${text}`);
    return {
      name,
      append: (text) => line(text),
      appendLine: (text) => line(text),
      clear() {},
      show() {},
      hide() {},
      dispose() {},
    };
  },
  get activeTextEditor() {
    return activeEditor;
  },
  get visibleTextEditors() {
    return [...visibleEditors.values()];
  },
  // The window is always the one in front, there are no terminals or notebooks to list, and the
  // theme is dark until the host is told otherwise.
  state: { focused: true, active: true },
  terminals: [],
  activeTerminal: undefined,
  visibleNotebookEditors: [],
  activeNotebookEditor: undefined,
  activeColorTheme: { kind: 2 },
  tabGroups: { all: [], activeTabGroup: { tabs: [], viewColumn: 1, isActive: true }, onDidChangeTabs: neverFires, onDidChangeTabGroups: neverFires },
  onDidChangeActiveTextEditor: (listener, thisArg, disposables) => activeEditorChanged.event(listener, thisArg, disposables),
  onDidChangeVisibleTextEditors: (listener, thisArg, disposables) => visibleEditorsChanged.event(listener, thisArg, disposables),
  onDidChangeTextEditorSelection: (listener, thisArg, disposables) => selectionChanged.event(listener, thisArg, disposables),
  showTextDocument,
  // Runs the task and shows how it goes: in a notification, or (for a window or source control
  // location) in the status bar line. `report({message, increment})` changes the text.
  async withProgress(options, task) {
    const o = options || {};
    const where = typeof o.location === 'object' && o.location ? 15 : o.location;
    const title = String(o.title || '');
    const id = nextRequest++;
    const show = (text, done) => (where === 15 ? send({ type: 'progress', id, text, done }) : text && send({ type: 'status', text }));
    show(title, false);
    const progress = {
      report(value) {
        const text = [title, value && value.message].filter(Boolean).join(': ');
        if (text) show(text, false);
      },
    };
    const source = new CancellationTokenSource();
    try {
      return await task(progress, source.token);
    } finally {
      show(title, true);
      source.dispose();
    }
  },
  // Items in the status bar. RustCode draws the text (icons written `$(name)` become a symbol or
  // nothing) and runs the command when one is clicked.
  createStatusBarItem(alignmentOrId, priorityOrAlignment, maybePriority) {
    const named = typeof alignmentOrId === 'string';
    const alignment = named ? priorityOrAlignment : alignmentOrId;
    const priority = named ? maybePriority : priorityOrAlignment;
    return new StatusBarItem(nextRequest++, alignment ?? 1, priority ?? 0);
  },
  setStatusBarMessage(text) {
    send({ type: 'status', text: String(text) });
    return new Disposable(() => {});
  },
  // Backgrounds, text colors and text after a line are drawn; borders, outlines, gutter icons and
  // the overview ruler are accepted and left out.
  createTextEditorDecorationType(options) {
    const color = wireColor;
    const key = `decoration-${nextRequest++}`;
    // Borders and outlines are written as one CSS value (`1px solid red`) or in parts.
    const edge = (whole, parts) => {
      const text = typeof whole === 'string' ? whole : '';
      const found = /(#[0-9a-fA-F]{3,8}|rgba?\([^)]*\)|hsla?\([^)]*\)|[a-zA-Z]+)\s*$/.exec(text);
      const width = /(\d+(?:\.\d+)?)px/.exec(text);
      const colorValue = color(parts.color) || (found && !/^(solid|dashed|dotted|double|none|px)$/.test(found[1]) ? found[1] : null);
      return colorValue ? { color: colorValue, width: width ? Number(width[1]) : parts.width ? parseFloat(parts.width) || 1 : 1, style: /dashed|dotted/.exec(text)?.[0] ?? parts.style ?? 'solid' } : null;
    };
    const style = {
      background: color(options.backgroundColor),
      color: color(options.color),
      wholeLine: !!options.isWholeLine,
      border: edge(options.border, { color: options.borderColor, width: options.borderWidth, style: options.borderStyle }),
      outline: edge(options.outline, { color: options.outlineColor, width: options.outlineWidth, style: options.outlineStyle }),
      rounded: !!options.borderRadius,
    };
    return { key, options, style, dispose: () => send({ type: 'decorationsDrop', key }) };
  },
};

class StatusBarItem {
  constructor(id, alignment, priority) {
    this._id = id;
    this.alignment = alignment;
    this.priority = priority;
    this._text = '';
    this._tooltip = undefined;
    this._command = undefined;
    this._visible = false;
    this._pending = false;
    this.name = undefined;
    this.color = undefined;
    this.backgroundColor = undefined;
    this.accessibilityInformation = undefined;
  }
  get id() {
    return String(this._id);
  }
  _changed() {
    if (!this._visible || this._pending) return;
    // One message for several changes made together.
    this._pending = true;
    queueMicrotask(() => {
      this._pending = false;
      if (this._visible) this._send();
    });
  }
  _send() {
    const command = this._command;
    send({
      type: 'statusItem',
      id: this._id,
      text: this._text,
      tooltip: this._tooltip && (typeof this._tooltip === 'string' ? this._tooltip : this._tooltip.value),
      command: command && (typeof command === 'string' ? command : command.command),
      arguments: command && typeof command === 'object' ? command.arguments || [] : [],
      left: this.alignment !== 2,
      priority: this.priority,
    });
  }
  get text() {
    return this._text;
  }
  set text(value) {
    this._text = String(value ?? '');
    this._changed();
  }
  get tooltip() {
    return this._tooltip;
  }
  set tooltip(value) {
    this._tooltip = value;
    this._changed();
  }
  get command() {
    return this._command;
  }
  set command(value) {
    this._command = value;
    this._changed();
  }
  show() {
    this._visible = true;
    this._send();
  }
  hide() {
    if (!this._visible) return;
    this._visible = false;
    send({ type: 'statusItemGone', id: this._id });
  }
  dispose() {
    this.hide();
  }
}

const workspace = {
  get workspaceFolders() {
    return workspaceFolders.length ? workspaceFolders : undefined;
  },
  get name() {
    return workspaceFolders[0] && workspaceFolders[0].name;
  },
  getConfiguration(section) {
    const full = (key) => (section ? (key ? `${section}.${key}` : section) : key);
    // The values of the section are also members of the object (`config.enable`), as in VS Code.
    const values = lookup(section || '');
    return Object.assign(values && typeof values === 'object' ? JSON.parse(JSON.stringify(values)) : {}, {
      get: (key, fallback) => {
        const value = lookup(full(key));
        return value === undefined ? fallback : value;
      },
      has: (key) => lookup(full(key)) !== undefined,
      // The default as the extension declared it, and the user's value when it differs from it.
      inspect: (key) => {
        const value = lookup(full(key));
        const defaultValue = lookup(full(key), declaredDefaults);
        const userSet = value !== undefined && JSON.stringify(value) !== JSON.stringify(defaultValue);
        return {
          key: full(key), defaultValue, globalValue: userSet ? value : undefined,
          workspaceValue: undefined, workspaceFolderValue: undefined,
          defaultLanguageValue: undefined, globalLanguageValue: undefined, workspaceLanguageValue: undefined, workspaceFolderLanguageValue: undefined,
          languageIds: undefined,
        };
      },
      // Saved by RustCode in the user's or the workspace's settings; the extension sees the
      // change at once. The target is a ConfigurationTarget, or true (user) / false (workspace).
      update: (key, value, target) => {
        const name = full(key);
        if (!name) return Promise.resolve();
        const next = { ...configuration };
        if (value === undefined) delete next[name];
        else next[name] = value;
        const where = target === 1 || target === true ? 'user' : target === 2 || target === 3 || target === false ? 'workspace' : 'auto';
        send({ type: 'setting', key: name, value: value === undefined ? null : value, remove: value === undefined, target: where });
        configure(next);
        return Promise.resolve();
      },
    });
  },
  // Every folder is trusted: RustCode has no restricted mode.
  isTrusted: true,
  onDidGrantWorkspaceTrust: neverFires,
  onDidChangeConfiguration: (listener, thisArg, disposables) => configurationChanged.event(listener, thisArg, disposables),
  createFileSystemWatcher,
  onDidChangeWorkspaceFolders: neverFires,
  onDidOpenTextDocument: (listener, thisArg, disposables) => documentOpened.event(listener, thisArg, disposables),
  onDidCloseTextDocument: (listener, thisArg, disposables) => documentClosed.event(listener, thisArg, disposables),
  onDidChangeTextDocument: (listener, thisArg, disposables) => documentChanged.event(listener, thisArg, disposables),
  onDidSaveTextDocument: (listener, thisArg, disposables) => documentSaved.event(listener, thisArg, disposables),
  // Runs when a file is saved by hand (not by auto save). `event.waitUntil(thenable)` takes
  // TextEdit[] to make before the file is written.
  onWillSaveTextDocument(listener, thisArg, disposables) {
    const subscription = willSave.event(listener, thisArg, disposables);
    announceWillSave();
    return new Disposable(() => {
      subscription.dispose();
      announceWillSave();
    });
  },
  get textDocuments() {
    return [...documents.values()];
  },
  notebookDocuments: [],
  workspaceFile: undefined,
  get rootPath() {
    return workspaceFolders[0] && workspaceFolders[0].uri.fsPath;
  },
  openTextDocument,
  // `provider.provideTextDocumentContent(uri)` gives the text of documents with this scheme;
  // `openTextDocument(uri)` reads it, and `showTextDocument` shows it in an untitled editor.
  registerTextDocumentContentProvider(scheme, provider) {
    contentProviders.set(scheme, provider);
    const subscription = provider.onDidChange
      ? provider.onDidChange(async (uri) => {
          contentProviderChanged.fire(uri);
          // A document of this provider that is on screen shows the new text.
          const key = uri && uri.toString();
          if (!key || !shownVirtual.has(key)) return;
          try {
            const text = String((await provider.provideTextDocumentContent(uri, { isCancellationRequested: false })) ?? '');
            const known = detached.get(key);
            if (known) known._set({ path: key, languageId: known.languageId, version: known.version + 1, text, dirty: false });
            send({ type: 'showVirtual', title: shownVirtual.get(key), content: text, refresh: true });
          } catch (error) {
            log(describe(error));
          }
        })
      : undefined;
    return new Disposable(() => {
      contentProviders.delete(scheme);
      if (subscription) subscription.dispose();
    });
  },
  // Resolves to whether the changes were made. Open files change in the editor, unsaved; other
  // files are changed on disk.
  applyEdit: (edit) => requestEdit(edit._order),
  getWorkspaceFolder(uri) {
    const file = uri.fsPath;
    return workspaceFolders.find((f) => file === f.uri.fsPath || file.startsWith(f.uri.fsPath.replace(/\/$/, '') + '/'));
  },
  asRelativePath(what, includeWorkspaceFolder) {
    const file = typeof what === 'string' ? what : what.uri ? what.uri.fsPath : what.fsPath;
    const folder = workspace.getWorkspaceFolder(Uri.file(file));
    if (!folder) return file;
    const relative = path.relative(folder.uri.fsPath, file);
    return includeWorkspaceFolder && workspaceFolders.length > 1 ? `${folder.name}/${relative}` : relative;
  },
  fs: {
    readFile: async (uri) => new Uint8Array(await require('fs').promises.readFile(uri.fsPath)),
    writeFile: async (uri, content) => require('fs').promises.writeFile(uri.fsPath, content),
    createDirectory: async (uri) => {
      await require('fs').promises.mkdir(uri.fsPath, { recursive: true });
    },
    delete: async (uri, options) => require('fs').promises.rm(uri.fsPath, { recursive: !!(options && options.recursive), force: false }),
    rename: async (from, to) => require('fs').promises.rename(from.fsPath, to.fsPath),
    copy: async (from, to) => require('fs').promises.cp(from.fsPath, to.fsPath, { recursive: true }),
    stat: async (uri) => {
      const info = await require('fs').promises.lstat(uri.fsPath);
      const type = info.isSymbolicLink() ? 64 : info.isDirectory() ? 2 : info.isFile() ? 1 : 0;
      return { type, ctime: info.ctimeMs, mtime: info.mtimeMs, size: info.size };
    },
    readDirectory: async (uri) => {
      const entries = await require('fs').promises.readdir(uri.fsPath, { withFileTypes: true });
      return entries.map((e) => [e.name, e.isSymbolicLink() ? 64 : e.isDirectory() ? 2 : e.isFile() ? 1 : 0]);
    },
  },
};

// @@SERVICES@@

// @@COMPAT@@

const configurationChanged = new EventEmitter();

const base = {
  version: '1.105.0',
  commands,
  window,
  workspace,
  extensions: extensionsApi,
  env: {
    appName: 'RustCode', appRoot: '', appHost: 'desktop', language: 'en', machineId: 'rustcode', sessionId: 'rustcode', uriScheme: 'rustcode',
    shell: process.env.SHELL || '', remoteName: undefined, isTelemetryEnabled: false, isNewAppInstall: false, uiKind: 1, logLevel: 3,
    onDidChangeTelemetryEnabled: neverFires, onDidChangeShell: neverFires, onDidChangeLogLevel: neverFires,
    openExternal: async () => false, asExternalUri: async (uri) => uri,
    clipboard: { readText: async () => '', writeText: async () => {} },
    createTelemetryLogger: telemetryLogger,
  },
  ...serviceExports,
  ...compatExports,
  Disposable,
  EventEmitter,
  Uri,
  Position,
  Range,
  Selection,
  ThemeColor,
  SnippetString,
  CodeLens,
  TypeHierarchyItem,
  CallHierarchyItem,
  CallHierarchyIncomingCall,
  CallHierarchyOutgoingCall,
  DocumentLink,
  SymbolKind,
  SymbolTag,
  SymbolInformation,
  DocumentSymbol,
  FoldingRange,
  FoldingRangeKind,
  SelectionRange,
  InlayHintKind,
  InlayHintLabelPart,
  InlayHint,
  DocumentHighlight,
  DocumentHighlightKind,
  Color,
  ColorInformation,
  ColorPresentation,
  SemanticTokensLegend,
  SemanticTokens,
  SemanticTokensEdit,
  SemanticTokensEdits,
  SemanticTokensBuilder,
  ThemeIcon,
  TreeItem,
  TreeItemCollapsibleState,
  CancellationError,
  FileSystemError,
  RelativePattern,
  ...miscEnums,
  DiagnosticSeverity,
  DiagnosticTag,
  Diagnostic,
  DiagnosticRelatedInformation,
  Location,
  MarkdownString,
  Hover,
  CompletionItem,
  CompletionItemKind,
  CompletionList,
  CompletionTriggerKind,
  CodeAction,
  CodeActionKind,
  CodeActionTriggerKind,
  ParameterInformation,
  SignatureInformation,
  SignatureHelp,
  SignatureHelpTriggerKind,
  CancellationTokenSource,
  InlineCompletionItem,
  InlineCompletionList,
  InlineCompletionTriggerKind,
  languages,
  TextEdit,
  WorkspaceEdit,
  EndOfLine,
  TextEditorSelectionChangeKind,
  TextDocumentChangeReason,
  TextDocumentSaveReason,
  FileType: { Unknown: 0, File: 1, Directory: 2, SymbolicLink: 64 },
  TextEditorRevealType: { Default: 0, InCenter: 1, InCenterIfOutsideViewport: 2, AtTop: 3 },
  StatusBarAlignment: { Left: 1, Right: 2 },
  ViewColumn: { Active: -1, Beside: -2, One: 1, Two: 2, Three: 3 },
  ExtensionMode: { Production: 1, Development: 2, Test: 3 },
  ConfigurationTarget: { Global: 1, Workspace: 2, WorkspaceFolder: 3 },
};

// The classes of the API that are not modelled exist all the same (see `addLenientClasses`).
// This comes after `knownExports` is read, below.

// What is not modelled throws when called, naming the member.
function withUnsupportedStubs(target, prefix) {
  return new Proxy(target, {
    get(object, key) {
      if (key in object) return object[key];
      // What bundlers and `await` probe for is not a member (`__esModule` would make them read `.default`).
      if (typeof key === 'symbol' || key === 'then' || key === 'toJSON' || key === '__esModule' || key === 'default' || key === 'inspect') return undefined;
      return unsupportedMember(prefix + String(key));
    },
  });
}

const vscodeMembers = {
  ...base,
  commands: withUnsupportedStubs(commands, 'vscode.commands.'),
  window: withUnsupportedStubs(window, 'vscode.window.'),
  workspace: withUnsupportedStubs(workspace, 'vscode.workspace.'),
  languages: withUnsupportedStubs(languages, 'vscode.languages.'),
};
const vscode = withUnsupportedStubs(vscodeMembers, 'vscode.');

// ES modules (`"type": "module"`, `import {commands} from 'vscode'`) do not go through `Module._load`.
// A loader hook gives them a module that re-exports the object above, name by name: an import of
// a name the module lacks would fail to link, so every name VS Code's API has is listed, and the
// ones RustCode does not model become members that throw when used.
const knownExports = `
  version env commands window workspace languages extensions debug tasks scm authentication notebooks comments tests l10n chat lm
  CancellationError CancellationTokenSource CodeAction CodeActionKind CodeActionTriggerKind CodeLens Color ColorInformation ColorPresentation
  ColorThemeKind CommentMode CommentThreadCollapsibleState CommentThreadState CompletionItem CompletionItemKind CompletionItemTag CompletionList
  CompletionTriggerKind ConfigurationTarget CustomExecution DataTransfer DataTransferFile DataTransferItem DebugAdapterExecutable
  DebugAdapterInlineImplementation DebugAdapterNamedPipeServer DebugAdapterServer DebugConfigurationProviderTriggerKind DebugConsoleMode
  DecorationRangeBehavior Diagnostic DiagnosticRelatedInformation DiagnosticSeverity DiagnosticTag Disposable DocumentDropEdit DocumentHighlight
  DocumentHighlightKind DocumentLink DocumentPasteEdit DocumentPasteEditKind DocumentSymbol EndOfLine EnvironmentVariableMutatorType
  EvaluatableExpression EventEmitter ExtensionKind ExtensionMode ExternalUriOpenerPriority FileChangeType FileDecoration FileSystemError FileType
  FoldingRange FoldingRangeKind FunctionBreakpoint Hover InlayHint InlayHintKind InlayHintLabelPart InlineCompletionItem InlineCompletionList
  InlineCompletionTriggerKind InlineValueEvaluatableExpression InlineValueText InlineValueVariableLookup LanguageStatusSeverity
  LinkedEditingRanges Location LogLevel MarkdownString NotebookCellData NotebookCellKind NotebookCellOutput NotebookCellOutputItem
  NotebookCellStatusBarAlignment NotebookCellStatusBarItem NotebookControllerAffinity NotebookData NotebookEdit NotebookEditorRevealType
  NotebookRange OverviewRulerLane ParameterInformation PortAutoForwardAction Position ProcessExecution ProgressLocation QuickInputButtons
  QuickPickItemKind Range RelativePattern Selection SelectionRange SemanticTokens SemanticTokensBuilder SemanticTokensEdit SemanticTokensEdits
  SemanticTokensLegend ShellExecution ShellQuoting SignatureHelp SignatureHelpTriggerKind SignatureInformation SnippetString SnippetTextEdit
  SourceBreakpoint StandardTokenType StatusBarAlignment SymbolInformation SymbolKind SymbolTag Task TaskGroup TaskPanelKind TaskRevealKind
  TaskScope TerminalLink TerminalLocation TerminalProfile TerminalExitReason TextDocumentChangeReason TextDocumentSaveReason TextEdit
  TextEditorCursorStyle TextEditorLineNumbersStyle TextEditorRevealType TextEditorSelectionChangeKind ThemeColor ThemeIcon TreeItem
  TreeItemCheckboxState TreeItemCollapsibleState TypeHierarchyItem UIKind Uri ViewColumn WorkspaceEdit CallHierarchyItem
  CallHierarchyIncomingCall CallHierarchyOutgoingCall InputBoxValidationSeverity ChatResultFeedbackKind
`.split(/\s+/).filter(Boolean);

function installEsmHook() {
  const names = [...new Set([...knownExports, ...Object.keys(base)])].filter((n) => /^[A-Za-z_$][\w$]*$/.test(n) && n !== 'default');
  const source = `const v = globalThis.__rustcodeVscode;\nexport default v;\n${names.map((n) => `export const ${n} = v[${JSON.stringify(n)}];`).join('\n')}\n`;
  const resolve = (specifier, context, next) => (specifier === 'vscode' ? { url: 'rustcode:vscode', shortCircuit: true } : next(specifier, context));
  const load = (url, context, next) => (url === 'rustcode:vscode' ? { format: 'module', source, shortCircuit: true } : next(url, context));
  if (typeof Module.registerHooks === 'function') Module.registerHooks({ resolve, load });
  // Older Node: hooks that run beside the program, as a module made on the spot.
  else if (typeof Module.register === 'function') {
    const hooks = `export async function resolve(s, c, n) { return s === 'vscode' ? { url: 'rustcode:vscode', shortCircuit: true } : n(s, c); }\n` +
      `export async function load(u, c, n) { return u === 'rustcode:vscode' ? { format: 'module', source: ${JSON.stringify(source)}, shortCircuit: true } : n(u, c); }`;
    Module.register('data:text/javascript,' + encodeURIComponent(hooks));
  }
}

addLenientClasses(vscodeMembers, knownExports);
addLenientClasses(base, knownExports);
const originalLoad = Module._load;
globalThis.__rustcodeVscode = vscode;
installEsmHook();
Module._load = function (request, ...rest) {
  if (request === 'vscode') return vscode;
  return originalLoad.call(this, request, ...rest);
};
// Bun does not call `Module._load`; it lets a plugin provide a module by name.
if (typeof Bun !== 'undefined' && Bun.plugin) {
  Bun.plugin({
    name: 'vscode',
    setup(build) {
      build.module('vscode', () => ({ exports: vscode, loader: 'object' }));
    },
  });
}

// ---- extensions --------------------------------------------------------------------------------

const extensions = new Map(); // id -> {info, activation: Promise|undefined}
const owner = new Map(); // command id -> extension id

function activate(id) {
  const entry = extensions.get(id);
  if (!entry) return Promise.reject(new Error(`no extension ${id}`));
  if (!entry.activation) {
    entry.activation = (async () => {
      const { info } = entry;
      if (!info.main) {
        entry.done = true;
        return;
      }
      const exported = require(path.resolve(info.path, info.main));
      const context = makeContext(info);
      if (exported && typeof exported.activate === 'function') entry.api = await exported.activate(context);
      entry.done = true;
    })();
  }
  return entry.activation;
}

async function execute(message) {
  try {
    let handler = handlers.get(message.command);
    if (!handler) {
      const id = owner.get(message.command);
      if (id) await activate(id);
      handler = handlers.get(message.command);
    }
    if (!handler) throw new Error(`command '${message.command}' has no handler (is it registered in activate?)`);
    await handler(...resolveArguments(message.args));
    send({ type: 'result', id: message.id });
  } catch (error) {
    send({ type: 'result', id: message.id, error: describe(error) });
  }
}

function init(message) {
  configuration = message.configuration || {};
  declaredDefaults = message.defaults || {};
  workspaceFolders = (message.folders || []).map((folder, index) => ({
    uri: Uri.file(folder),
    name: path.basename(folder),
    index,
  }));
  for (const info of message.extensions) {
    try {
      const manifest = JSON.parse(fsSync.readFileSync(path.join(info.path, 'package.json'), 'utf8'));
      Object.assign(contributedDefaults, manifest.contributes && manifest.contributes.configurationDefaults);
    } catch (error) {
      // No manifest to read: nothing declared.
    }
    extensions.set(info.id, { info, activation: undefined });
    for (const command of info.commands || []) owner.set(command, info.id);
    for (const view of info.views || []) viewOwner.set(view, info.id);
    for (const type of info.taskTypes || []) taskTypeOwner.set(type, info.id);
    for (const type of info.customEditors || []) customEditorOwner.set(type, info.id);
    for (const event of info.activationEvents || []) {
      if (event.startsWith('onWebviewPanel:')) webviewPanelOwner.set(event.slice('onWebviewPanel:'.length), info.id);
    }
    // `*` and `onStartupFinished` ask to run as soon as the host starts.
    if ((info.activationEvents || []).some((event) => event === '*' || event === 'onStartupFinished')) {
      activate(info.id).catch((error) => send({ type: 'message', level: 'error', text: `${info.id}: ${describe(error)}` }));
    }
  }
}

// Messages about the workbench are handled one at a time, in order.
let workbenchQueue = Promise.resolve();
const later = (work) => {
  workbenchQueue = workbenchQueue.then(work).catch((error) => log(describe(error)));
};

function settleEdit({ request, applied }) {
  const resolve = waitingEdits.get(request);
  waitingEdits.delete(request);
  if (resolve) resolve(!!applied);
}

function settleSave({ path: filePath }) {
  const document = documents.get(filePath);
  if (document) documentSaved.fire(document);
}

process.on('uncaughtException', (error) => send({ type: 'message', level: 'error', text: describe(error) }));
// A promise nobody waits for failing is not for the user to read, as in VS Code: it goes to the log.
process.on('unhandledRejection', (error) => log(`Unhandled rejection: ${describe(error)}`));
console.log = (...args) => log(args.join(' '));
console.error = console.warn = console.info = console.log;

readline.createInterface({ input: process.stdin }).on('line', (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    return log(`bad message: ${line}`);
  }
  if (message.type === 'init') init(message);
  else if (message.type === 'execute') void execute(message);
  else if (message.type === 'configuration') configure(message.values, message.defaults);
  else if (message.type === 'workbench') later(() => updateWorkbench(message));
  else if (message.type === 'editResult') later(() => settleEdit(message));
  else if (message.type === 'saved') later(() => settleSave(message));
  else if (message.type === 'willSave') later(() => runWillSave(message));
  else if (message.type === 'promptAnswer') later(() => settlePrompt(message));
  else if (message.type === 'provide') later(() => void runProvide(message));
  else serviceMessage(message);
});
