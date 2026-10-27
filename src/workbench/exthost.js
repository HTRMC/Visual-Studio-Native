// vsn's extension host: runs JS extensions against a `vscode` module shim and
// talks to vsn in newline-delimited JSON on stdin/stdout, under Node.
// Extensions' console output goes to
// stderr, which vsn shows as the Extension Host output channel.
'use strict';
const Module = require('module');
const fs = require('fs');
const os = require('os');
const path = require('path');
const readline = require('readline');
const util = require('util');

const send = (m) => process.stdout.write(JSON.stringify(m) + '\n');
const log = (...a) => process.stderr.write(util.format(...a) + '\n');
for (const k of ['log', 'info', 'warn', 'error', 'debug', 'trace']) console[k] = log;
const errorText = (e) => (e && e.stack) || String(e);
process.on('uncaughtException', (e) => send({ m: 'error', text: errorText(e) }));
process.on('unhandledRejection', (e) => log('Unhandled rejection:', errorText(e)));

// ---- plain classes ---------------------------------------------------------

class Disposable {
  constructor(fn) { this._fn = fn; }
  static from(...ds) { return new Disposable(() => ds.forEach((d) => d && d.dispose && d.dispose())); }
  dispose() { const fn = this._fn; this._fn = undefined; if (fn) fn(); }
}

class EventEmitter {
  constructor() {
    this._listeners = new Set();
    this.event = (fn, thisArg, disposables) => {
      const f = thisArg ? fn.bind(thisArg) : fn;
      this._listeners.add(f);
      const d = new Disposable(() => this._listeners.delete(f));
      if (Array.isArray(disposables)) disposables.push(d);
      return d;
    };
  }
  fire(e) {
    for (const f of [...this._listeners]) {
      try { f(e); } catch (err) { log(errorText(err)); }
    }
  }
  dispose() { this._listeners.clear(); }
}
// An event that never fires, for the ones vsn doesn't report yet.
const never = new EventEmitter().event;

class Uri {
  constructor(scheme, authority, p, query, fragment) {
    Object.assign(this, { scheme, authority: authority || '', path: p || '', query: query || '', fragment: fragment || '' });
  }
  static file(p) {
    let s = p.replace(/\\/g, '/');
    if (!s.startsWith('/')) s = '/' + s;
    return new Uri('file', '', s);
  }
  static parse(s) {
    const m = /^([a-zA-Z][\w+.-]*):(?:\/\/([^/?#]*))?([^?#]*)(?:\?([^#]*))?(?:#(.*))?$/.exec(s);
    if (!m) return Uri.file(s);
    return new Uri(m[1], m[2], decodeURIComponent(m[3]), m[4], m[5]);
  }
  static joinPath(uri, ...parts) { return uri.with({ path: path.posix.join(uri.path, ...parts) }); }
  static from(c) { return new Uri(c.scheme, c.authority, c.path, c.query, c.fragment); }
  get fsPath() {
    const p = /^\/[a-zA-Z]:/.test(this.path) ? this.path.slice(1) : this.path;
    return process.platform === 'win32' ? p.replace(/\//g, '\\') : p;
  }
  with(c) { return Uri.from({ ...this, ...c }); }
  toString() {
    const q = this.query ? '?' + this.query : '';
    const f = this.fragment ? '#' + this.fragment : '';
    return `${this.scheme}://${this.authority}${encodeURI(this.path)}${q}${f}`;
  }
  toJSON() { return { scheme: this.scheme, authority: this.authority, path: this.path, query: this.query, fragment: this.fragment, fsPath: this.fsPath }; }
}

class Position {
  constructor(line, character) { this.line = line; this.character = character; }
  translate(dl = 0, dc = 0) { return new Position(this.line + dl, this.character + dc); }
  with(line = this.line, character = this.character) { return new Position(line, character); }
  isEqual(o) { return this.line === o.line && this.character === o.character; }
  isBefore(o) { return this.line < o.line || (this.line === o.line && this.character < o.character); }
  isAfter(o) { return !this.isEqual(o) && !this.isBefore(o); }
}

class Range {
  constructor(a, b, c, d) {
    if (typeof a === 'number') { a = new Position(a, b); b = new Position(c, d); }
    [this.start, this.end] = b.isBefore(a) ? [b, a] : [a, b];
  }
  get isEmpty() { return this.start.isEqual(this.end); }
  get isSingleLine() { return this.start.line === this.end.line; }
  isEqual(o) { return this.start.isEqual(o.start) && this.end.isEqual(o.end); }
  contains(x) {
    if (x instanceof Range) return this.contains(x.start) && this.contains(x.end);
    return !x.isBefore(this.start) && !this.end.isBefore(x);
  }
  with(start = this.start, end = this.end) {
    if (!(start instanceof Position)) ({ start = this.start, end = this.end } = start);
    return new Range(start, end);
  }
}

class Selection extends Range {
  constructor(a, b, c, d) {
    super(a, b, c, d);
    if (typeof a === 'number') { a = new Position(a, b); b = new Position(c, d); }
    this.anchor = a; this.active = b;
  }
  get isReversed() { return this.active.isBefore(this.anchor); }
}

class ThemeColor { constructor(id) { this.id = id; } }
class ThemeIcon { constructor(id, color) { this.id = id; this.color = color; } }
ThemeIcon.File = new ThemeIcon('file');
ThemeIcon.Folder = new ThemeIcon('folder');

class CancellationTokenSource {
  constructor() {
    const e = new EventEmitter();
    this.token = { isCancellationRequested: false, onCancellationRequested: e.event };
    this._e = e;
  }
  cancel() { this.token.isCancellationRequested = true; this._e.fire(); }
  dispose() { this._e.dispose(); }
}

class Location {
  constructor(uri, r) { this.uri = uri; this.range = r instanceof Position ? new Range(r, r) : r; }
}
class Diagnostic {
  constructor(range, message, severity = 0) { Object.assign(this, { range, message, severity }); }
}
class DiagnosticRelatedInformation { constructor(location, message) { Object.assign(this, { location, message }); } }
class MarkdownString {
  constructor(value = '', supportThemeIcons) { this.value = value; this.supportThemeIcons = supportThemeIcons; }
  appendText(s) { this.value += s.replace(/[\\`*_{}[\]()#+\-.!]/g, '\\$&'); return this; }
  appendMarkdown(s) { this.value += s; return this; }
  appendCodeblock(code, lang = '') { this.value += '\n```' + lang + '\n' + code + '\n```\n'; return this; }
}
class Hover { constructor(contents, range) { this.contents = Array.isArray(contents) ? contents : [contents]; this.range = range; } }
class CodeActionKind {
  constructor(value) { this.value = value; }
  append(part) { return new CodeActionKind(this.value ? `${this.value}.${part}` : part); }
  contains(o) { return !this.value || this.value === o.value || o.value.startsWith(this.value + '.'); }
  intersects(o) { return this.contains(o) || o.contains(this); }
}
for (const [k, v] of Object.entries({
  Empty: '', QuickFix: 'quickfix', Refactor: 'refactor', RefactorExtract: 'refactor.extract', RefactorInline: 'refactor.inline',
  RefactorMove: 'refactor.move', RefactorRewrite: 'refactor.rewrite', Source: 'source',
  SourceOrganizeImports: 'source.organizeImports', SourceFixAll: 'source.fixAll', Notebook: 'notebook',
})) CodeActionKind[k] = new CodeActionKind(v);
class SnippetString { constructor(value = '') { this.value = value; } }
class CompletionItem { constructor(label, kind) { this.label = label; this.kind = kind; } }
class CompletionList { constructor(items = [], isIncomplete = false) { this.items = items; this.isIncomplete = isIncomplete; } }
class TextEdit {
  constructor(range, newText) { this.range = range; this.newText = newText; }
  static replace(r, t) { return new TextEdit(r, t); }
  static insert(p, t) { return new TextEdit(new Range(p, p), t); }
  static delete(r) { return new TextEdit(r, ''); }
}
class WorkspaceEdit {
  constructor() { this._edits = new Map(); }
  _for(uri) {
    const k = uri.toString();
    if (!this._edits.has(k)) this._edits.set(k, [uri, []]);
    return this._edits.get(k)[1];
  }
  get size() { return this._edits.size; }
  set(uri, edits) { this._edits.set(uri.toString(), [uri, (edits || []).map((e) => (Array.isArray(e) ? e[0] : e))]); }
  get(uri) { return (this._edits.get(uri.toString()) || [uri, []])[1]; }
  has(uri) { return this._edits.has(uri.toString()); }
  entries() { return [...this._edits.values()]; }
  replace(uri, r, t) { this._for(uri).push(new TextEdit(r, t)); }
  insert(uri, p, t) { this._for(uri).push(TextEdit.insert(p, t)); }
  delete(uri, r) { this._for(uri).push(TextEdit.delete(r)); }
  // ponytail: file operations are dropped; vsn applies text edits only.
  createFile() {}
  deleteFile() {}
  renameFile() {}
}
class CodeAction { constructor(title, kind) { this.title = title; this.kind = kind; } }
class SignatureHelp { constructor() { this.signatures = []; this.activeSignature = 0; this.activeParameter = 0; } }
class SignatureInformation { constructor(label, documentation) { this.label = label; this.documentation = documentation; this.parameters = []; } }
class ParameterInformation { constructor(label, documentation) { this.label = label; this.documentation = documentation; } }

// A document vsn has open, as vscode.TextDocument.
class TextDocument {
  constructor(uri, languageId, version, text) {
    Object.assign(this, { uri: Uri.parse(uri), languageId, version, isClosed: false, isDirty: false, isUntitled: false, eol: 1 });
    this.setText(text);
  }
  setText(text) {
    this._text = text;
    this._lines = text.split(/\r?\n/);
    this._starts = [0];
    for (let i = 0; i < text.length; i++) if (text[i] === '\n') this._starts.push(i + 1);
  }
  get fileName() { return this.uri.fsPath; }
  get lineCount() { return this._lines.length; }
  async save() { return false; }
  getText(r) { return r ? this._text.slice(this.offsetAt(r.start), this.offsetAt(r.end)) : this._text; }
  lineAt(l) {
    if (typeof l !== 'number') l = l.line;
    const text = this._lines[l];
    const first = text.search(/\S/);
    return {
      lineNumber: l, text, range: new Range(l, 0, l, text.length),
      rangeIncludingLineBreak: new Range(l, 0, l + 1, 0),
      firstNonWhitespaceCharacterIndex: first < 0 ? text.length : first, isEmptyOrWhitespace: first < 0,
    };
  }
  offsetAt(p) {
    const l = Math.max(0, Math.min(p.line, this._lines.length - 1));
    return this._starts[l] + Math.max(0, Math.min(p.character, this._lines[l].length));
  }
  positionAt(o) {
    o = Math.max(0, Math.min(o, this._text.length));
    let l = this._starts.length - 1;
    while (this._starts[l] > o) l--;
    return new Position(l, Math.min(o - this._starts[l], this._lines[l].length));
  }
  validatePosition(p) { return this.positionAt(this.offsetAt(p)); }
  validateRange(r) { return new Range(this.validatePosition(r.start), this.validatePosition(r.end)); }
  getWordRangeAtPosition(p, re = /[\w$]+/g) {
    const text = this.lineAt(p.line).text;
    for (const m of text.matchAll(new RegExp(re.source, 'g'))) {
      if (m.index <= p.character && p.character <= m.index + m[0].length) return new Range(p.line, m.index, p.line, m.index + m[0].length);
    }
    return undefined;
  }
}

// ---- state from vsn ----------------------------------------------------------

let settings = {};
let defaults = {};
let root = null;
// VS Code's commands vsn runs itself, for getCommands.
let builtins = [];
// One array, as vscode-languageclient diffs folders by identity.
let folders;
const extensions = new Map(); // id -> { id, dir, manifest, exports, active }
const commands = new Map();
// The documents of the languages extensions serve, by the uri vsn sent.
const documents = new Map();
const opened = new EventEmitter();
const changed = new EventEmitter();
const closed = new EventEmitter();
const saved = new EventEmitter();
const activeChanged = new EventEmitter();
// Each language feature's providers: { selector, provider }.
const providers = { hover: [], completion: [], definition: [], formatting: [], rangeFormatting: [], codeAction: [], rename: [], references: [], signatureHelp: [] };

// ---- the vscode API ----------------------------------------------------------

const vscodeCommands = {
  registerCommand(id, fn, thisArg) {
    commands.set(id, thisArg ? fn.bind(thisArg) : fn);
    return new Disposable(() => commands.delete(id));
  },
  registerTextEditorCommand(id, fn, thisArg) {
    // Runs with the active editor and an edit builder, applied after.
    return vscodeCommands.registerCommand(id, (...args) => {
      const editor = vscodeWindow.activeTextEditor;
      return editor && editor.edit((b) => fn.call(thisArg, editor, b, ...args));
    });
  },
  async executeCommand(id, ...args) {
    if (id === 'setContext') {
      send({ m: 'setContext', key: args[0], value: args[1] ?? null });
      return undefined;
    }
    // What a key typed in the editor does: replace each selection.
    const editor = vscodeWindow.activeTextEditor;
    if (id === 'type' || id === 'default:type') {
      return editor && editor.edit((b) => editor.selections.forEach((s) => b.replace(s, args[0]?.text ?? '')));
    }
    const fn = commands.get(id);
    if (fn) return fn(...args);
    send({ m: 'execute', command: id, args });
    return undefined;
  },
  async getCommands(filterInternal) {
    return [...commands.keys(), ...builtins, ...(filterInternal ? [] : ['setContext', 'type'])];
  },
};

// vsn's answers to the host's questions (quick picks, input boxes, message
// buttons), by id. They skip the queue: a command may be awaiting one.
const asking = new Map();
let nextAsk = 0;
function ask(msg) {
  const id = ++nextAsk;
  send({ ...msg, id });
  return new Promise((resolve) => asking.set(id, resolve));
}

// Codicons in labels (`$(check) Done`) as plain text.
const plain = (s) => String(s || '').replace(/\$\([\w~-]+\)\s*/g, '');

function message(severity) {
  // (message, options?, ...items); items are strings or { title }.
  // ponytail: `modal` messages are toasts too, and `detail` isn't shown.
  return async (text, ...items) => {
    if (items[0] && typeof items[0] === 'object' && !('title' in items[0])) items.shift();
    items = items.filter((i) => i !== undefined);
    const buttons = items.map((i) => (typeof i === 'string' ? i : String(i.title)));
    if (!buttons.length) { send({ m: 'message', severity, text: String(text) }); return undefined; }
    const ix = await ask({ m: 'message', severity, text: String(text), buttons });
    return ix == null ? undefined : items[ix];
  };
}

async function showQuickPick(items, options = {}) {
  // ponytail: one pick even with `canPickMany`, and onDidSelectItem never fires.
  const picks = ((await items) || []).filter((i) => typeof i === 'string' || i.kind !== -1);
  if (!picks.length) return undefined;
  const rows = picks.map((i) => (typeof i === 'string' ? [i, ''] : [plain(i.label), plain(i.description)]));
  const ix = await ask({ m: 'quickPick', placeholder: options.placeHolder || options.title || '', rows });
  if (ix == null) return undefined;
  return options.canPickMany ? [picks[ix]] : picks[ix];
}

async function showInputBox(options = {}) {
  // ponytail: the palette's input can't mask, so a password box reads as
  // cancelled rather than showing the secret; validateInput isn't run.
  if (options.password) return undefined;
  const value = await ask({ m: 'inputBox', placeholder: options.placeHolder || options.prompt || '', value: options.value || '' });
  return value == null ? undefined : value;
}

let nextItem = 0;
function createStatusBarItem(a, b, c) {
  // (alignment?, priority?) or (id, alignment?, priority?)
  if (typeof a !== 'string') [a, b, c] = [undefined, a, b];
  const id = a || `item-${++nextItem}`;
  const fields = { text: '', tooltip: undefined, command: undefined, name: undefined, color: undefined, backgroundColor: undefined, accessibilityInformation: undefined };
  let visible = false;
  const sync = () => {
    let command = fields.command;
    let args = [];
    if (command && typeof command === 'object') { args = command.arguments || []; command = command.command; }
    const tooltip = fields.tooltip && typeof fields.tooltip === 'object' ? fields.tooltip.value : fields.tooltip;
    send({ m: 'status', id, visible, text: fields.text, tooltip: tooltip || '', command: command || null, args, alignment: b || 1, priority: c || 0 });
  };
  const item = {
    id, alignment: b || 1, priority: c,
    show() { visible = true; sync(); },
    hide() { visible = false; sync(); },
    dispose() { visible = false; sync(); },
  };
  for (const k of Object.keys(fields)) {
    Object.defineProperty(item, k, {
      get: () => fields[k],
      set: (v) => { fields[k] = v; if (visible) sync(); },
    });
  }
  return item;
}

function createOutputChannel(name) {
  const write = (text) => send({ m: 'log', channel: name, text: String(text) });
  const level = (l) => (s, ...a) => write(`[${l}] ${util.format(s, ...a)}`);
  // ponytail: append starts a new line, and show() doesn't reveal the panel.
  return {
    name,
    append: write,
    appendLine: write,
    replace: () => {}, clear: () => {}, show: () => {}, hide: () => {}, dispose: () => {},
    trace: level('trace'), debug: level('debug'), info: level('info'),
    warn: level('warning'), error: level('error'),
    logLevel: 3, onDidChangeLogLevel: never,
  };
}

const vscodeWindow = {
  // No notebooks, and tabs only as far as vsn tells extensions of them.
  activeNotebookEditor: undefined, visibleNotebookEditors: [],
  tabGroups: { all: [], activeTabGroup: { tabs: [], isActive: true, viewColumn: 1 }, onDidChangeTabGroups: never, onDidChangeTabs: never, close: async () => false },
  showInformationMessage: message('info'),
  showWarningMessage: message('warning'),
  showErrorMessage: message('error'),
  showQuickPick,
  showInputBox,
  showOpenDialog: async () => undefined,
  showSaveDialog: async () => undefined,
  createStatusBarItem,
  createOutputChannel,
  createTreeView,
  registerTreeDataProvider,
  setStatusBarMessage(text) {
    const item = createStatusBarItem(1, -Infinity);
    item.text = text; item.show();
    return new Disposable(() => item.dispose());
  },
  withProgress: async (_options, task) => task({ report() {} }, new CancellationTokenSource().token),
  activeTextEditor: undefined,
  visibleTextEditors: [],
  onDidChangeActiveTextEditor: activeChanged.event,
  terminals: [],
  state: { focused: true },
};

function configuration(section) {
  const full = (key) => (section ? (key ? `${section}.${key}` : section) : key);
  const lookup = (key) => {
    const k = full(key);
    if (k in settings) return settings[k];
    if (k in defaults) return defaults[k];
    // A section read as an object: its keys' values, nested by dots.
    const prefix = k ? k + '.' : '';
    let obj;
    for (const src of [defaults, settings]) {
      for (const [name, value] of Object.entries(src)) {
        if (!name.startsWith(prefix)) continue;
        obj = obj || {};
        let o = obj;
        const parts = name.slice(prefix.length).split('.');
        parts.slice(0, -1).forEach((p) => { o = o[p] = o[p] || {}; });
        o[parts[parts.length - 1]] = value;
      }
    }
    return obj;
  };
  // Its values are its properties too, as of now, as in VS Code.
  return {
    ...lookup(),
    get(key, defaultValue) { const v = lookup(key); return v === undefined ? defaultValue : v; },
    has(key) { return lookup(key) !== undefined; },
    inspect(key) { const k = full(key); return { key: k, defaultValue: defaults[k], globalValue: settings[k] }; },
    async update(key, value) {
      const k = full(key);
      if (value === undefined) delete settings[k]; else settings[k] = value;
      send({ m: 'setting', key: k, value: value === undefined ? null : value });
      // Resolves a turn later, as VS Code's round trip to its main process
      // does; the Zig extension's setup needs that time for libsodium to load.
      await new Promise((r) => setImmediate(r));
    },
  };
}

// A glob as VS Code's (`**`, `*`, `?`, `{a,b}`, `[...]`), matching a
// '/'-separated relative path.
function globRe(glob) {
  let re = '';
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === '*' && glob[i + 1] === '*') {
      re += glob[i + 2] === '/' ? '(?:.*/)?' : '.*';
      i += glob[i + 2] === '/' ? 2 : 1;
    } else if (c === '*') re += '[^/]*';
    else if (c === '?') re += '[^/]';
    else if (c === '{') re += '(?:';
    else if (c === '}') re += ')';
    else if (c === ',') re += '|';
    else if (c === '[' || c === ']') re += c;
    else re += c.replace(/[.+^$()|\\]/g, '\\$&');
  }
  return new RegExp(`^${re}$`, process.platform === 'win32' ? 'i' : '');
}

class RelativePattern {
  constructor(base, pattern) {
    this.baseUri = typeof base === 'string' ? Uri.file(base) : base.uri || base;
    this.base = this.baseUri.fsPath;
    this.pattern = pattern;
  }
}

// ponytail: walks the folder on each call, skipping `files.exclude` (not
// `search.exclude` or .gitignore); an index if extensions call it often.
async function findFiles(include, exclude, maxResults = Infinity, token) {
  const base = include?.base || root;
  if (!base) return [];
  const inc = globRe(include?.pattern ?? include);
  const fromSettings = { '**/.git': true, '**/.svn': true, '**/.hg': true, '**/CVS': true, '**/.DS_Store': true, '**/Thumbs.db': true, ...settings['files.exclude'] };
  const excl = (exclude === undefined ? Object.keys(fromSettings).filter((k) => fromSettings[k] === true) : exclude ? [exclude.pattern ?? exclude] : []).map(globRe);
  const skip = (rel) => excl.some((r) => r.test(rel));
  const found = [];
  const walk = async (dir, rel) => {
    let entries = [];
    try { entries = await fs.promises.readdir(dir, { withFileTypes: true }); } catch {}
    for (const e of entries) {
      if (found.length >= maxResults || token?.isCancellationRequested) return;
      const r = rel ? `${rel}/${e.name}` : e.name;
      if (skip(r)) continue;
      if (e.isDirectory()) await walk(path.join(dir, e.name), r);
      else if (inc.test(r)) found.push(Uri.file(path.join(dir, e.name)));
    }
  };
  await walk(base, '');
  return found;
}

// VS Code's own strings are English here: `t` fills in `{0}` or `{name}`.
const l10n = {
  bundle: undefined, uri: undefined,
  t(message, ...args) {
    if (typeof message === 'object') ({ message, args = [] } = message);
    const named = args.length === 1 && typeof args[0] === 'object' ? args[0] : args;
    return String(message).replace(/\{(\w+)\}/g, (m, k) => (named[k] === undefined ? m : String(named[k])));
  },
};

const vscodeWorkspace = {
  notebookDocuments: [],
  getConfiguration: configuration,
  findFiles,
  get workspaceFolders() { return folders; },
  get rootPath() { return root || undefined; },
  get name() { return root ? path.basename(root) : undefined; },
  workspaceFile: undefined,
  getWorkspaceFolder(uri) { return this.workspaceFolders && uri.fsPath.toLowerCase().startsWith(root.toLowerCase()) ? this.workspaceFolders[0] : undefined; },
  asRelativePath(p) { const f = typeof p === 'string' ? p : p.fsPath; return root ? path.relative(root, f).replace(/\\/g, '/') : f; },
  get textDocuments() { return [...documents.values()]; },
  onDidOpenTextDocument: opened.event,
  onDidChangeTextDocument: changed.event,
  onDidCloseTextDocument: closed.event,
  onDidSaveTextDocument: saved.event,
  // ponytail: always applied; vsn doesn't say whether the edit held.
  async applyEdit(edit) {
    send({ m: 'applyEdit', edit: lspWorkspaceEdit(edit) });
    return true;
  },
  async openTextDocument(u) {
    // ponytail: files vsn hasn't opened are read from disk and never kept in sync.
    const uri = typeof u === 'string' ? Uri.file(u) : u;
    const doc = [...documents.values()].find((d) => d.uri.fsPath === uri.fsPath);
    return doc || new TextDocument(uri.toString(), path.extname(uri.fsPath).slice(1), 1, await fs.promises.readFile(uri.fsPath, 'utf8'));
  },
  // ponytail: no file events reach the host yet.
  createFileSystemWatcher: () => ({ onDidCreate: never, onDidChange: never, onDidDelete: never, dispose() {} }),
  isTrusted: true,
  onDidChangeConfiguration: never,
  // Node's errors come out as VS Code's, which extensions test by code.
  fs: Object.fromEntries(Object.entries({
    readFile: async (uri) => new Uint8Array(await fs.promises.readFile(uri.fsPath)),
    writeFile: async (uri, data) => fs.promises.writeFile(uri.fsPath, data),
    stat: async (uri) => { const s = await fs.promises.stat(uri.fsPath); return { type: s.isDirectory() ? 2 : 1, ctime: s.ctimeMs, mtime: s.mtimeMs, size: s.size }; },
    readDirectory: async (uri) => (await fs.promises.readdir(uri.fsPath, { withFileTypes: true })).map((d) => [d.name, d.isDirectory() ? 2 : 1]),
    createDirectory: async (uri) => fs.promises.mkdir(uri.fsPath, { recursive: true }),
    delete: async (uri, o) => fs.promises.rm(uri.fsPath, { recursive: !!(o && o.recursive), force: true }),
    copy: async (from, to, o) => fs.promises.cp(from.fsPath, to.fsPath, { recursive: true, force: !!o?.overwrite, errorOnExist: !o?.overwrite }),
    rename: async (from, to) => fs.promises.rename(from.fsPath, to.fsPath),
  }).map(([k, f]) => [k, (uri, ...a) => f(uri, ...a).catch((err) => {
    const code = { ENOENT: 'FileNotFound', EEXIST: 'FileExists', ERR_FS_CP_EEXIST: 'FileExists', ENOTDIR: 'FileNotADirectory', EISDIR: 'FileIsADirectory', EPERM: 'NoPermissions', EACCES: 'NoPermissions' }[err.code];
    throw code ? FileSystemError[code](uri) : err;
  })]).concat([['isWritableFileSystem', () => true]])),
};

const vscodeExtensions = {
  getExtension(id) {
    const e = extensions.get(String(id).toLowerCase());
    return e && { id: e.id, extensionPath: e.dir, extensionUri: Uri.file(e.dir), packageJSON: e.manifest, get isActive() { return !!e.isActive; }, get exports() { return e.exports; }, activate: () => activate(e.id).then(() => e.exports) };
  },
  get all() { return [...extensions.keys()].map((id) => vscodeExtensions.getExtension(id)); },
  onDidChange: never,
};

const env = {
  appName: 'Visual Studio Code Native', appRoot: path.dirname(process.execPath), appHost: 'desktop',
  language: 'en', machineId: os.hostname(), sessionId: String(process.pid), uriScheme: 'vsn',
  shell: process.env.ComSpec || '/bin/sh', uiKind: 1, remoteName: undefined, isTelemetryEnabled: false,
  onDidChangeTelemetryEnabled: never,
  // ponytail: no clipboard or browser reach from the host yet.
  clipboard: { readText: async () => '', writeText: async () => {} },
  openExternal: async () => false,
  asExternalUri: async (u) => u,
  // Telemetry is off, so the sender is never called.
  createTelemetryLogger: () => ({
    isUsageEnabled: false, isErrorsEnabled: false, onDidChangeEnableStates: never,
    logUsage() {}, logError() {}, dispose() {},
  }),
};

// ---- source control ----------------------------------------------------------

// Extensions' source controls by handle: the SourceControl, its groups and
// its input box's fields. Any change to them is sent whole to vsn.
const scms = new Map();
let nextScm = 0;

const watched = (obj, sync) => new Proxy(obj, { set(t, k, v) { t[k] = v; sync(); return true; } });

function scmJson(handle) {
  const { sc, groups, input } = scms.get(handle);
  const resource = (r) => {
    const d = r.decorations || {};
    const tip = d.tooltip && typeof d.tooltip === 'object' ? d.tooltip.value : d.tooltip;
    return { path: r.resourceUri.fsPath, tooltip: tip || '', strike: !!d.strikeThrough, faded: !!d.faded, context: r.contextValue || '' };
  };
  return {
    m: 'scm', handle, id: sc.id, label: sc.label, root: sc.rootUri ? sc.rootUri.fsPath : null,
    input: input.value, placeholder: input.placeholder, count: sc.count,
    groups: groups.map((g) => ({ id: g.id, label: g.label, hideWhenEmpty: !!g.hideWhenEmpty, resources: (g.resourceStates || []).map(resource) })),
  };
}

// ponytail: quickDiffProvider, commitTemplate, statusBarCommands and
// inputBox.validateInput are kept but unused.
function createSourceControl(id, label, rootUri) {
  const handle = ++nextScm;
  let pending = false;
  const sync = () => {
    if (pending) return;
    pending = true;
    queueMicrotask(() => { pending = false; if (scms.has(handle)) send(scmJson(handle)); });
  };
  const input = { value: '', placeholder: '', enabled: true, visible: true };
  const groups = [];
  const sc = watched({
    id, label, rootUri, count: undefined, inputBox: watched(input, sync),
    createResourceGroup(gid, glabel) {
      const g = watched({
        id: gid, label: glabel, hideWhenEmpty: undefined, resourceStates: [],
        dispose() { groups.splice(groups.indexOf(g), 1); sync(); },
      }, sync);
      groups.push(g);
      sync();
      return g;
    },
    dispose() { scms.delete(handle); send({ m: 'scm', handle, gone: true }); },
  }, sync);
  scms.set(handle, { sc, groups, input });
  sync();
  return sc;
}

// The SourceControl, group or resource state a `{ $scm: [h, g?, r?] }`
// argument from vsn's menus names.
function scmArg([h, g, r]) {
  const s = scms.get(h);
  if (!s || g === undefined) return s && s.sc;
  const group = s.groups[g];
  return r === undefined ? group : group && group.resourceStates[r];
}

function runScm(cmd, args = []) {
  if (cmd) vscodeCommands.executeCommand(cmd.command, ...(cmd.arguments || []), ...args).catch((err) => send({ m: 'error', text: errorText(err) }));
}

// ---- tree views --------------------------------------------------------------

class TreeItem {
  constructor(arg, collapsibleState = 0) {
    if (arg instanceof Uri) this.resourceUri = arg; else this.label = arg;
    this.collapsibleState = collapsibleState;
  }
}

// Extensions' tree views by view id: the provider, the TreeView, the nodes
// last sent by handle and which are open by key. Any change sends the whole
// tree to vsn.
// ponytail: every open node's children are fetched again on each change, and
// onDidChangeTreeData(element) refreshes the whole tree; fine for small trees.
const trees = new Map();

function treeIcon(icon, item) {
  if (icon && typeof icon === 'object' && 'dark' in icon) icon = icon.dark;
  const file = item.resourceUri && item.resourceUri.fsPath;
  if (icon instanceof ThemeIcon) {
    if (file && (icon.id === 'file' || icon.id === 'folder')) return { file, folder: icon.id === 'folder' };
    return { codicon: icon.id };
  }
  if (icon) return { path: icon instanceof Uri ? icon.fsPath : String(icon) };
  return file ? { file, folder: !!item.collapsibleState } : null;
}

async function treeJson(id) {
  const t = trees.get(id);
  const handles = [];
  const walk = async (el, parent, depth) => {
    const out = [];
    for (const child of (await t.provider.getChildren(el)) || []) {
      const item = await t.provider.getTreeItem(child);
      const uri = item.resourceUri;
      const label = typeof item.label === 'object' && item.label ? item.label.label : item.label ?? (uri ? path.basename(uri.fsPath) : '');
      const key = parent + '/' + (item.id ?? label);
      const open = !!item.collapsibleState && (t.open.get(key) ?? item.collapsibleState === 2);
      const tip = item.tooltip && typeof item.tooltip === 'object' ? item.tooltip.value : item.tooltip;
      const h = handles.push({ el: child, item, key, open }) - 1;
      out.push({
        h, depth, label: String(label), description: typeof item.description === 'string' ? item.description : '',
        tooltip: tip || '', icon: treeIcon(item.iconPath, item), context: item.contextValue || '',
        state: item.collapsibleState ? (open ? 2 : 1) : 0,
      });
      if (open) out.push(...(await walk(child, key, depth + 1)));
    }
    return out;
  };
  let items = [];
  try { items = await walk(undefined, '', 0); } catch (err) { log(errorText(err)); }
  t.handles = handles;
  t.items = items;
  return treeMessage(t, id);
}

const treeMessage = (t, id, v = t.view || {}) =>
  ({ m: 'tree', view: id, items: t.items, title: v.title ?? null, message: v.message ?? null, description: v.description ?? null });

function refreshTree(id) {
  const t = trees.get(id);
  if (!t || t.pending) return;
  t.pending = true;
  t.chain = t.chain.then(() => { t.pending = false; return trees.get(id) === t && treeJson(id).then(send); });
}

function registerTreeDataProvider(id, provider) {
  const t = { provider, view: undefined, handles: [], items: [], open: new Map(), pending: false, chain: Promise.resolve() };
  trees.set(id, t);
  const sub = provider.onDidChangeTreeData && provider.onDidChangeTreeData(() => refreshTree(id));
  refreshTree(id);
  return new Disposable(() => {
    if (sub) sub.dispose();
    if (trees.get(id) === t) { trees.delete(id); send({ m: 'tree', view: id, gone: true }); }
  });
}

// ponytail: reveal, selection, visibility, expand/collapse and checkbox
// events aren't reported.
function createTreeView(id, options) {
  const d = registerTreeDataProvider(id, options.treeDataProvider);
  const view = watched({
    title: undefined, message: undefined, description: undefined, badge: undefined, visible: true, selection: [],
    onDidChangeVisibility: never, onDidChangeSelection: never, onDidExpandElement: never, onDidCollapseElement: never,
    onDidChangeCheckboxState: never, reveal: async () => {}, dispose: () => d.dispose(),
  // Its title or message only, without fetching the items again: a
  // getChildren() that sets them would otherwise run forever.
  }, () => trees.get(id)?.view && send(treeMessage(trees.get(id), id)));
  trees.get(id).view = view;
  return view;
}

// The element a `{ $tree: [view, h] }` argument from vsn's menus names.
function treeArg([view, h]) {
  const n = trees.get(view)?.handles[h];
  return n && n.el;
}

// ---- languages ---------------------------------------------------------------

// The language ids a document selector names.
const selectorLanguages = (s) => (Array.isArray(s) ? s.flatMap(selectorLanguages) : [typeof s === 'string' ? s : s && s.language].filter(Boolean));

function match(selector, doc) {
  if (Array.isArray(selector)) return Math.max(0, ...selector.map((s) => match(s, doc)));
  if (typeof selector === 'string') return selector === '*' || selector === doc.languageId ? 10 : 0;
  // ponytail: `pattern` and `notebookType` filters aren't checked.
  if (selector.language && selector.language !== '*' && selector.language !== doc.languageId) return 0;
  if (selector.scheme && selector.scheme !== '*' && selector.scheme !== doc.uri.scheme) return 0;
  return 10;
}

function register(feature) {
  return (selector, provider) => {
    const entry = { selector, provider };
    providers[feature].push(entry);
    // vsn sends this language's documents here from now on.
    send({ m: 'languages', ids: selectorLanguages(selector) });
    return new Disposable(() => providers[feature].splice(providers[feature].indexOf(entry), 1));
  };
}

// vscode's objects as LSP's, for vsn.
const lspRange = (r) => r && { start: { line: r.start.line, character: r.start.character }, end: { line: r.end.line, character: r.end.character } };
const markdown = (c) => (typeof c === 'string' ? c : c.language ? '```' + c.language + '\n' + c.value + '\n```' : c.value);
const lspLocation = (l) => (l.targetUri
  ? { targetUri: l.targetUri.toString(), targetRange: lspRange(l.targetRange), targetSelectionRange: lspRange(l.targetSelectionRange || l.targetRange), originSelectionRange: lspRange(l.originSelectionRange) }
  : { targetUri: l.uri.toString(), targetRange: lspRange(l.range), targetSelectionRange: lspRange(l.range) });

function lspCompletion(c) {
  const label = typeof c.label === 'string' ? c.label : c.label.label;
  let text = c.insertText === undefined ? label : typeof c.insertText === 'string' ? c.insertText : c.insertText.value;
  // ponytail: vsn's editor takes no snippets, so tab stops become plain text.
  if (c.insertText instanceof SnippetString) text = text.replace(/\$\{\d+:([^}]*)\}/g, '$1').replace(/\$\{?\d+\}?/g, '');
  const r = c.range && (c.range.replacing || c.range);
  const doc = c.documentation;
  return {
    label, kind: c.kind === undefined ? undefined : c.kind + 1, detail: c.detail,
    documentation: doc && (typeof doc === 'string' ? doc : { kind: 'markdown', value: doc.value }),
    sortText: c.sortText, filterText: c.filterText,
    ...(r ? { textEdit: { range: lspRange(r), newText: text } } : { insertText: text }),
  };
}

const lspDiagnostic = (d) => ({
  range: lspRange(d.range), message: d.message, severity: (d.severity || 0) + 1, source: d.source,
  code: d.code && typeof d.code === 'object' ? d.code.value : d.code,
});

function createDiagnosticCollection(name) {
  const map = new Map();
  const publish = (key) => send({ m: 'lsp', msg: { method: 'textDocument/publishDiagnostics', params: { uri: key, diagnostics: (map.get(key) || []).map(lspDiagnostic) } } });
  // ponytail: two collections reporting one file overwrite each other in vsn.
  const c = {
    name,
    set(uri, ds) {
      if (Array.isArray(uri)) return uri.forEach(([u, d]) => c.set(u, d));
      map.set(uri.toString(), ds || []);
      publish(uri.toString());
    },
    delete(uri) { map.delete(uri.toString()); publish(uri.toString()); },
    clear() { for (const k of [...map.keys()]) { map.delete(k); publish(k); } },
    get: (uri) => map.get(uri.toString()),
    has: (uri) => map.has(uri.toString()),
    forEach(fn) { for (const [k, v] of map) fn(Uri.parse(k), v, c); },
    dispose() { c.clear(); },
    [Symbol.iterator]: function* () { for (const [k, v] of map) yield [Uri.parse(k), v]; },
  };
  return c;
}

const vscodeLanguages = {
  match,
  createDiagnosticCollection,
  getDiagnostics: () => [],
  async getLanguages() { return [...new Set([...extensions.values()].flatMap((e) => ((e.manifest.contributes || {}).languages || []).map((l) => l.id)))]; },
  registerHoverProvider: register('hover'),
  registerCompletionItemProvider: register('completion'),
  registerDefinitionProvider: register('definition'),
  registerDocumentFormattingEditProvider: register('formatting'),
  registerDocumentRangeFormattingEditProvider: register('rangeFormatting'),
  registerCodeActionsProvider: register('codeAction'),
  registerRenameProvider: register('rename'),
  registerReferenceProvider: register('references'),
  registerSignatureHelpProvider: register('signatureHelp'),
  onDidChangeDiagnostics: never,
};

// LSP's objects as vscode's, for the extensions.
const vsRange = (r) => new Range(r.start.line, r.start.character, r.end.line, r.end.character);
function vsDiagnostic(d) {
  const v = new Diagnostic(vsRange(d.range), d.message, (d.severity || 1) - 1);
  return Object.assign(v, { source: d.source, code: d.code });
}

// The commands of the last code actions sent, which can carry arguments
// JSON can't, so vsn gets their index instead.
let actionCommands = [];
function lspCommand(c) {
  if (!c) return undefined;
  actionCommands.push(c);
  return { title: c.title, command: '_vsn.codeAction', arguments: [actionCommands.length - 1] };
}
function lspWorkspaceEdit(edit) {
  return edit && { changes: Object.fromEntries(edit.entries().map(([uri, edits]) => [uri.toString(), edits.map((e) => ({ range: lspRange(e.range), newText: e.newText }))])) };
}
function lspCodeAction(a) {
  // A plain Command rather than a CodeAction.
  if (typeof a.command === 'string') return { title: a.title, command: lspCommand(a) };
  return { title: a.title, kind: a.kind && a.kind.value, isPreferred: a.isPreferred, edit: lspWorkspaceEdit(a.edit), command: lspCommand(a.command) };
}

// vsn's requests for a document, answered by the extensions' providers.
async function provide(method, params) {
  if (method === 'workspace/executeCommand') {
    const c = actionCommands[params.arguments[0]];
    return c && vscodeCommands.executeCommand(c.command, ...(c.arguments || []));
  }
  const doc = documents.get(params.textDocument.uri);
  let feature = { 'textDocument/hover': 'hover', 'textDocument/completion': 'completion', 'textDocument/definition': 'definition', 'textDocument/formatting': 'formatting', 'textDocument/rangeFormatting': 'rangeFormatting', 'textDocument/codeAction': 'codeAction', 'textDocument/rename': 'rename', 'textDocument/references': 'references', 'textDocument/signatureHelp': 'signatureHelp' }[method];
  const find = (f) => doc && f && providers[f].find((p) => match(p.selector, doc));
  let entry = find(feature);
  // Format Document falls back on formatting the whole range.
  if (!entry && feature === 'formatting') [entry, feature] = [find('rangeFormatting'), 'rangeFormatting'];
  if (!entry) return null;
  const p = entry.provider;
  const token = new CancellationTokenSource().token;
  const pos = params.position && new Position(params.position.line, params.position.character);
  switch (feature) {
    case 'hover': {
      const h = await p.provideHover(doc, pos, token);
      return h && { contents: { kind: 'markdown', value: [].concat(h.contents).map(markdown).join('\n\n---\n\n') }, range: lspRange(h.range) };
    }
    case 'completion': {
      const r = await p.provideCompletionItems(doc, pos, token, { triggerKind: 0 });
      return r && { isIncomplete: !!r.isIncomplete, items: (Array.isArray(r) ? r : r.items).map(lspCompletion) };
    }
    case 'definition': {
      const r = await p.provideDefinition(doc, pos, token);
      return r && (Array.isArray(r) ? r : [r]).map(lspLocation);
    }
    case 'formatting': {
      const r = await p.provideDocumentFormattingEdits(doc, params.options, token);
      return r && r.map((e) => ({ range: lspRange(e.range), newText: e.newText }));
    }
    case 'rangeFormatting': {
      const range = params.range ? vsRange(params.range) : new Range(new Position(0, 0), doc.positionAt(doc._text.length));
      const r = await p.provideDocumentRangeFormattingEdits(doc, doc.validateRange(range), params.options, token);
      return r && r.map((e) => ({ range: lspRange(e.range), newText: e.newText }));
    }
    case 'codeAction': {
      // ponytail: only the first matching provider answers, and actions
      // needing resolveCodeAction for their edit apply nothing.
      const context = { diagnostics: params.context.diagnostics.map(vsDiagnostic), only: undefined, triggerKind: 1 };
      const r = await p.provideCodeActions(doc, vsRange(params.range), context, token);
      actionCommands = [];
      return r && r.filter((a) => !a.disabled).map(lspCodeAction);
    }
    case 'rename':
      return lspWorkspaceEdit(await p.provideRenameEdits(doc, pos, params.newName, token));
    case 'references': {
      const r = await p.provideReferences(doc, pos, params.context, token);
      return r && r.map((l) => ({ uri: l.uri.toString(), range: lspRange(l.range) }));
    }
    case 'signatureHelp': {
      const r = await p.provideSignatureHelp(doc, pos, token, { triggerKind: 1, isRetrigger: false });
      const text = (d) => d && (typeof d === 'string' ? d : { kind: 'markdown', value: markdown(d) });
      return r && {
        activeSignature: r.activeSignature, activeParameter: r.activeParameter,
        signatures: r.signatures.map((s) => ({
          label: s.label, documentation: text(s.documentation), activeParameter: s.activeParameter,
          parameters: (s.parameters || []).map((q) => ({ label: q.label, documentation: text(q.documentation) })),
        })),
      };
    }
  }
  return null;
}

// vsn talks to the host as to a language server for the languages it serves.
function onLsp(msg) {
  const p = msg.params || {};
  const td = p.textDocument || {};
  switch (msg.method) {
    case 'textDocument/didOpen': {
      const doc = new TextDocument(td.uri, td.languageId, td.version, td.text);
      documents.set(td.uri, doc);
      return opened.fire(doc);
    }
    case 'textDocument/didChange': {
      const doc = documents.get(td.uri);
      if (!doc) return;
      const old = doc._text;
      const range = new Range(new Position(0, 0), doc.positionAt(old.length));
      doc.setText(p.contentChanges[0].text);
      doc.version = td.version;
      return changed.fire({ document: doc, contentChanges: [{ range, rangeOffset: 0, rangeLength: old.length, text: doc._text }], reason: undefined });
    }
    case 'textDocument/didSave':
      return documents.has(td.uri) && saved.fire(documents.get(td.uri));
    case 'textDocument/didClose': {
      const doc = documents.get(td.uri);
      if (!doc) return;
      documents.delete(td.uri);
      doc.isClosed = true;
      return closed.fire(doc);
    }
  }
  if (msg.id === undefined) return;
  provide(msg.method, p).then(
    (result) => send({ m: 'lsp', msg: { jsonrpc: '2.0', id: msg.id, result: result === undefined ? null : result } }),
    (err) => { log(errorText(err)); send({ m: 'lsp', msg: { jsonrpc: '2.0', id: msg.id, result: null } }); },
  );
}

// vsn's active editor, as window.activeTextEditor: sent when the tab changes
// and before each command, with its text and selection then.
// ponytail: the selection and the text of a file no language server of the
// host's has open aren't sent as they change in between.
function setActiveEditor(e) {
  let editor;
  if (e) {
    const doc = documents.get(e.uri) || new TextDocument(e.uri, e.languageId, 1, e.text);
    const [s, end] = e.selection;
    const selection = new Selection(s.line, s.character, end.line, end.character);
    editor = {
      document: doc, selection, selections: [selection], visibleRanges: [],
      options: { tabSize: 4, insertSpaces: true }, viewColumn: 1,
      async edit(cb) {
        const edit = new WorkspaceEdit();
        cb({
          replace: (r, t) => edit.replace(doc.uri, r instanceof Position ? new Range(r, r) : r, t),
          insert: (p, t) => edit.insert(doc.uri, p, t),
          delete: (r) => edit.delete(doc.uri, r),
          setEndOfLine() {},
        });
        return vscodeWorkspace.applyEdit(edit);
      },
      // ponytail: tab stops are dropped, placeholders kept as text.
      async insertSnippet(snippet, at) {
        const text = String(snippet.value ?? snippet).replace(/\$\{\d+:([^}]*)\}/g, '$1').replace(/\$\{?\d+\}?/g, '');
        return this.edit((b) => b.replace(at || selection, text));
      },
      setDecorations() {}, revealRange() {}, show() {}, hide() {},
    };
  }
  const was = vscodeWindow.activeTextEditor;
  vscodeWindow.activeTextEditor = editor;
  vscodeWindow.visibleTextEditors = editor ? [editor] : [];
  if (was?.document.uri.toString() !== editor?.document.uri.toString()) activeChanged.fire(editor);
}

// The API members vsn has; any other namespace answers with no-op stubs so
// an extension that touches it still activates, and any other class name
// with an empty class it can extend. A stub's result is a stub too, so
// `createTestController().createRunProfile()` goes through.
const unimplemented = new Set();
const stubs = (name, set = new Map()) => new Proxy(() => {}, {
  set(_, k, v) { set.set(k, v); return true; },
  get(_, k) {
    if (set.has(k)) return set.get(k);
    // Iterates as empty, so `for (const x of stub)` runs.
    if (k === Symbol.iterator) return function* () {};
    // Reads as '' in a string, so `${stub}` and `stub + 'x'` don't throw.
    if (k === Symbol.toPrimitive) return () => '';
    if (typeof k !== 'string' || k === 'then') return undefined;
    if (k.startsWith('onDid') || k.startsWith('onWill')) return never;
    if (k === 'dispose') return () => {};
    // Ends a chain, so `while (x.parent) x = x.parent` stops.
    if (name.split('.').length > 8) return undefined;
    return stubs(`${name}.${k}`);
  },
  apply() {
    if (!unimplemented.has(name)) log(`vscode.${name} is not implemented`);
    unimplemented.add(name);
    return stubs(`${name}()`);
  },
});
// VS Code's enums, from its vscode.d.ts: `Name:A B C` counts from 0, `A=1`
// sets a value. Each also maps its values back to names, as TypeScript's do.
const ENUMS = `
TextEditorSelectionChangeKind:Keyboard=1 Mouse=2 Command=3
TextEditorCursorStyle:Line=1 Block=2 Underline=3 LineThin=4 BlockOutline=5 UnderlineThin=6
TextEditorLineNumbersStyle:Off On Relative Interval
TextEditorRevealType:Default InCenter InCenterIfOutsideViewport AtTop
OverviewRulerLane:Left=1 Center=2 Right=4 Full=7
DecorationRangeBehavior:OpenOpen ClosedClosed OpenClosed ClosedOpen
EndOfLine:LF=1 CRLF=2
QuickPickItemKind:Separator=-1 Default=0
InputBoxValidationSeverity:Info=1 Warning=2 Error=3
CodeActionTriggerKind:Invoke=1 Automatic=2
DocumentHighlightKind:Text Read Write
SymbolKind:File Module Namespace Package Class Method Property Field Constructor Enum Interface Function Variable Constant String Number Boolean Array Object Key Null EnumMember Struct Event Operator TypeParameter
SymbolTag:Deprecated=1
SignatureHelpTriggerKind:Invoke=1 TriggerCharacter=2 ContentChange=3
CompletionItemKind:Text Method Function Constructor Field Variable Class Interface Module Property Unit Value Enum Keyword Snippet Color File Reference Folder EnumMember Constant Struct Event Operator TypeParameter User Issue
CompletionItemTag:Deprecated=1
CompletionTriggerKind:Invoke TriggerCharacter TriggerForIncompleteCompletions
InlineCompletionTriggerKind:Invoke Automatic
InlayHintKind:Type=1 Parameter=2
FoldingRangeKind:Comment=1 Imports=2 Region=3
IndentAction:None Indent IndentOutdent Outdent
SyntaxTokenType:Other Comment String RegEx
ConfigurationTarget:Global=1 Workspace=2 WorkspaceFolder=3
DiagnosticSeverity:Error Warning Information Hint
DiagnosticTag:Unnecessary=1 Deprecated=2
LanguageStatusSeverity:Information Warning Error
ViewColumn:Active=-1 Beside=-2 One=1 Two=2 Three=3 Four=4 Five=5 Six=6 Seven=7 Eight=8 Nine=9
StatusBarAlignment:Left=1 Right=2
TerminalLocation:Panel=1 Editor=2
ExtensionKind:UI=1 Workspace=2
ExtensionMode:Production=1 Development=2 Test=3
ColorThemeKind:Light=1 Dark=2 HighContrast=3 HighContrastLight=4
TaskRevealKind:Always=1 Silent=2 Never=3
TaskPanelKind:Shared=1 Dedicated=2 New=3
ShellQuoting:Escape=1 Strong=2 Weak=3
TaskScope:Global=1 Workspace=2
FileType:Unknown=0 File=1 Directory=2 SymbolicLink=64
FilePermission:Readonly=1
FileChangeType:Changed=1 Created=2 Deleted=3
UIKind:Desktop=1 Web=2
LogLevel:Off Trace Debug Info Warning Error
TreeItemCollapsibleState:None Collapsed Expanded
TreeItemCheckboxState:Unchecked Checked
TerminalExitReason:Unknown Shutdown Process User Extension
EnvironmentVariableMutatorType:Replace=1 Append=2 Prepend=3
ProgressLocation:SourceControl=1 Window=10 Notification=15
TextDocumentChangeReason:Undo=1 Redo=2
TextDocumentSaveReason:Manual=1 AfterDelay=2 FocusOut=3
NotebookEditorRevealType:Default InCenter InCenterIfOutsideViewport AtTop
NotebookCellKind:Markup=1 Code=2
NotebookControllerAffinity:Default=1 Preferred=2
NotebookCellStatusBarAlignment:Left=1 Right=2
DebugConsoleMode:Separate MergeWithParent
DebugConfigurationProviderTriggerKind:Initial=1 Dynamic=2
CommentThreadCollapsibleState:Collapsed Expanded
CommentMode:Editing Preview
CommentThreadState:Unresolved Resolved
TestRunProfileKind:Run=1 Debug=2 Coverage=3
ChatResultFeedbackKind:Unhelpful Helpful
LanguageModelChatMessageRole:User=1 Assistant=2
LanguageModelChatToolMode:Auto=1 Required=2`;
const enums = Object.fromEntries(ENUMS.trim().split('\n').map((line) => {
  const [name, members] = line.split(':');
  const e = {};
  let v = -1;
  for (const m of members.split(' ')) {
    const [k, n] = m.split('=');
    v = n === undefined ? v + 1 : Number(n);
    e[(e[k] = v)] = k;
  }
  return [name, e];
}));

// The rest of VS Code's classes, keeping their constructor's arguments as
// these fields. ponytail: plain data; methods only where extensions call them
// at activation (add more as extensions need them).
const CLASSES = `CodeLens:range,command EvaluatableExpression:range,expression
InlineValueText:range,text InlineValueVariableLookup:range,variableName,caseSensitiveLookup
InlineValueEvaluatableExpression:range,expression DocumentHighlight:range,kind
SymbolInformation:name,kind,containerName,location DocumentSymbol:name,detail,kind,range,selectionRange
SnippetTextEdit:range,snippet NotebookEdit:range,newCells SemanticTokensLegend:tokenTypes,tokenModifiers
SemanticTokensBuilder:legend SemanticTokens:data,resultId SemanticTokensEdits:edits,resultId
SemanticTokensEdit:start,deleteCount,data InlineCompletionList:items InlineCompletionItem:insertText,range,command
DocumentLink:range,target Color:red,green,blue,alpha ColorInformation:range,color ColorPresentation:label
InlayHintLabelPart:value InlayHint:position,label,kind FoldingRange:start,end,kind SelectionRange:range,parent
CallHierarchyItem:kind,name,detail,uri,range,selectionRange CallHierarchyIncomingCall:from,fromRanges
CallHierarchyOutgoingCall:to,fromRanges TypeHierarchyItem:kind,name,detail,uri,range,selectionRange
LinkedEditingRanges:ranges,wordPattern DocumentDropEdit:insertText TerminalLink:startIndex,length,tooltip
TerminalProfile:options FileDecoration:badge,tooltip,color TaskGroup:id,label ProcessExecution:process,args,options
ShellExecution:commandLine,args,options CustomExecution:callback
Task:definition,scope,name,source,execution,problemMatchers DataTransferItem:value DataTransfer
QuickInputButtons NotebookRange:start,end NotebookCellOutputItem:data,mime NotebookCellOutput:items,metadata
NotebookCellData:kind,value,languageId NotebookData:cells NotebookCellStatusBarItem:text,alignment
DebugAdapterExecutable:command,args,options DebugAdapterServer:port,host DebugAdapterNamedPipeServer:path
DebugAdapterInlineImplementation:implementation Breakpoint:enabled,condition,hitCondition,logMessage
SourceBreakpoint:location,enabled,condition,hitCondition,logMessage
FunctionBreakpoint:functionName,enabled,condition,hitCondition,logMessage DebugThread:session,threadId
DebugStackFrame:session,threadId,frameId TestTag:id TestRunRequest:include,exclude,profile
TestMessageStackFrame:label,uri,position TestMessage:message TestCoverageCount:covered,total
FileCoverage:uri,statementCoverage,branchCoverage,declarationCoverage StatementCoverage:executed,location,branches
BranchCoverage:executed,location,label DeclarationCoverage:name,executed,location TabInputText:uri
TabInputTextDiff:original,modified TabInputCustom:uri,viewType TabInputWebview:viewType
TabInputNotebook:uri,notebookType TabInputNotebookDiff:original,modified,notebookType TabInputTerminal
TelemetryTrustedValue:value ChatRequestTurn ChatResponseTurn ChatResponseMarkdownPart ChatResponseFileTreePart
ChatResponseAnchorPart ChatResponseProgressPart ChatResponseReferencePart ChatResponseCommandButtonPart
LanguageModelChatMessage LanguageModelError LanguageModelToolCallPart LanguageModelToolResultPart
LanguageModelTextPart LanguageModelPromptTsxPart LanguageModelToolResult`;
const classes = Object.fromEntries(CLASSES.split(/\s+/).map((c) => {
  const [name, f] = c.split(':');
  const fields = f ? f.split(',') : [];
  return [name, { [name]: class { constructor(...a) { fields.forEach((k, i) => (this[k] = a[i])); } } }[name]];
}));
for (const [id, label] of [['clean', 'Clean'], ['build', 'Build'], ['rebuild', 'Rebuild'], ['test', 'Test']]) {
  classes.TaskGroup[label] = new classes.TaskGroup(id, label);
}
classes.QuickInputButtons.Back = { iconPath: new ThemeIcon('arrow-left') };

class CancellationError extends Error {
  constructor() { super('Canceled'); this.name = 'Canceled'; }
}
class FileSystemError extends Error {
  constructor(m, code = 'Unknown') { super(m && typeof m === 'object' ? String(m) : m); this.code = code; this.name = `${code} (FileSystemError)`; }
}
for (const code of ['FileNotFound', 'FileExists', 'FileNotADirectory', 'FileIsADirectory', 'NoPermissions', 'Unavailable']) {
  FileSystemError[code] = (m) => new FileSystemError(m, code);
}

// A namespace whose missing members are stubs.
const withStubs = (ns, obj) => new Proxy(obj, {
  get(t, k, r) { return k in t || typeof k !== 'string' || k === 'then' ? Reflect.get(t, k, r) : stubs(ns)[k]; },
});
const known = {
  ...enums, ...classes,
  version: '1.95.0',
  // TypeScript's `import * as vscode` then uses the module as is.
  __esModule: true,
  commands: vscodeCommands, extensions: vscodeExtensions, env, l10n, RelativePattern,
  window: withStubs('window', vscodeWindow), workspace: withStubs('workspace', vscodeWorkspace), languages: withStubs('languages', vscodeLanguages),
  scm: withStubs('scm', { createSourceControl }),
  // Present from the start: esbuild's `__toESM` copies only what's there.
  ...Object.fromEntries('debug tests tasks notebooks authentication comments chat lm interactive'.split(' ').map((n) => [n, stubs(n)])),
  Disposable, EventEmitter, Uri, Position, Range, Selection, ThemeColor, ThemeIcon, CancellationTokenSource,
  TreeItem, Location, Diagnostic, DiagnosticRelatedInformation, MarkdownString, Hover, SnippetString, CompletionItem, CompletionList, TextEdit, CodeActionKind,
  WorkspaceEdit, CodeAction, SignatureHelp, SignatureInformation, ParameterInformation, CancellationError, FileSystemError,
};
const vscode = new Proxy(known, {
  get(t, k) {
    if (k in t || typeof k !== 'string' || k === 'then') return t[k];
    if (/^[A-Z]/.test(k)) return (t[k] = class {});
    return (t[k] = stubs(k));
  },
});
// esbuild's `__toESM` takes an `__esModule` module's namespace from `default`.
known.default = vscode;

// `require('vscode')` from any extension gets the shim, through a file next
// to this one.
globalThis.__vsnVscode = vscode;
const shim = path.join(__dirname, 'vscode.js');
fs.writeFileSync(shim, 'module.exports = globalThis.__vsnVscode;\n');
const resolve = Module._resolveFilename;
Module._resolveFilename = function (request, ...rest) {
  return request === 'vscode' ? shim : resolve.call(this, request, ...rest);
};

// ---- activation --------------------------------------------------------------

// A Memento kept in a JSON file (VS Code uses SQLite), or in memory without one.
function memento(file) {
  let m = {};
  try { if (file) m = JSON.parse(fs.readFileSync(file, 'utf8')); } catch {}
  return {
    get: (k, d) => (Object.hasOwn(m, k) ? m[k] : d),
    // Setting undefined removes the key, as in VS Code.
    update: async (k, v) => {
      if (v === undefined) delete m[k]; else m[k] = v;
      if (!file) return;
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, JSON.stringify(m));
    },
    keys: () => Object.keys(m),
    setKeysForSync() {},
  };
}

async function activate(id) {
  const e = extensions.get(id);
  if (!e || e.active) return e?.activating;
  e.active = true;
  let done;
  e.activating = new Promise((r) => (done = r));
  // Its dependencies first, as VS Code does.
  await Promise.all((e.manifest.extensionDependencies || []).map((d) => activate(d.toLowerCase())));
  const storage = path.join(os.homedir(), '.vsn', 'globalStorage', id);
  // Per folder, as VS Code's workspaceStorage/<hash>; none without a folder.
  const hash = root && require('crypto').createHash('md5').update(root.toLowerCase()).digest('hex');
  const local = hash && path.join(os.homedir(), '.vsn', 'workspaceStorage', hash, id);
  const context = {
    subscriptions: [],
    extensionPath: e.dir,
    extensionUri: Uri.file(e.dir),
    asAbsolutePath: (rel) => path.join(e.dir, rel),
    globalState: memento(path.join(storage, 'state.json')),
    workspaceState: memento(local && path.join(local, 'state.json')),
    secrets: { get: async () => undefined, store: async () => {}, delete: async () => {}, onDidChange: never },
    storagePath: local || undefined, globalStoragePath: storage, logPath: storage,
    storageUri: local ? Uri.file(local) : undefined, globalStorageUri: Uri.file(storage), logUri: Uri.file(storage),
    extensionMode: 1,
    extension: vscodeExtensions.getExtension(id),
    environmentVariableCollection: stubs('environmentVariableCollection'),
  };
  try {
    const main = require(path.resolve(e.dir, e.manifest.main));
    e.deactivate = () => Promise.all([main.deactivate?.(), ...context.subscriptions.map((d) => d?.dispose?.())]);
    e.exports = main.activate ? await main.activate(context) : undefined;
    e.isActive = true;
    send({ m: 'activated', id });
  } catch (err) {
    send({ m: 'error', text: `Activating extension '${id}' failed: ${errorText(err)}` });
  }
  done();
}

async function run(command, args) {
  // Its extension may still be activating, as another's slow activate()
  // doesn't hold up the rest.
  if (!commands.has(command)) await Promise.all([...extensions.values()].map((e) => e.activating));
  const fn = commands.get(command);
  if (!fn) return send({ m: 'error', text: `command '${command}' not found` });
  try {
    // A menu passes its file as VS Code does, a Uri.
    // a source control's view its SourceControl, group or resource, and a
    // tree view its element.
    await fn(...(args || []).map((a) => (a && a.$uri ? Uri.parse(a.$uri) : a && a.$scm ? scmArg(a.$scm) : a && a.$tree ? treeArg(a.$tree) : a)));
  } catch (err) {
    send({ m: 'error', text: `Running the contributed command: '${command}' failed. ${errorText(err)}` });
  }
}

// Requests run in order; activations run side by side, as in VS Code.
let queue = Promise.resolve();
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', (line) => {
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.m === 'answer') {
    asking.get(msg.id)?.(msg.value);
    asking.delete(msg.id);
    return;
  }
  queue = queue.then(() => {
    switch (msg.m) {
      case 'init':
        settings = msg.settings || {};
        defaults = msg.defaults || {};
        root = msg.root || null;
        builtins = msg.builtins || [];
        folders = root ? [{ uri: Uri.file(root), name: path.basename(root), index: 0 }] : undefined;
        for (const e of msg.extensions || []) extensions.set(e.id, { ...e, active: false });
        return;
      case 'settings':
        settings = msg.settings || {};
        return;
      case 'activate':
        activate(msg.id);
        return;
      case 'run':
        // Not awaited: a command waiting on a quick pick mustn't hold up the rest.
        run(msg.command, msg.args);
        return;
      case 'lsp':
        return onLsp(msg.msg);
      case 'activeEditor':
        return setActiveEditor(msg.editor);
      case 'scmInput': {
        // Typed in vsn, so not sent back.
        const s = scms.get(msg.handle);
        if (s) s.input.value = msg.value;
        return;
      }
      case 'scmAccept': {
        const s = scms.get(msg.handle);
        return s && runScm(s.sc.acceptInputCommand);
      }
      case 'scmOpen': {
        // A resource without a command opens its file, as in VS Code.
        const r = scmArg(msg.ref);
        return r && runScm(r.command || { command: 'vscode.open', arguments: [r.resourceUri] });
      }
      case 'treeClick': {
        // A collapsible item toggles; any item runs its command.
        const t = trees.get(msg.view);
        const n = t && t.handles[msg.h];
        if (!n) return;
        if (n.item.collapsibleState) { t.open.set(n.key, !n.open); refreshTree(msg.view); }
        return runScm(n.item.command);
      }
    }
  });
});
// On shutdown or restart, as VS Code: each extension's deactivate() and
// subscriptions, given up to 5s together.
rl.on('close', () => {
  const all = [...extensions.values()].map((e) => e.deactivate && Promise.resolve().then(e.deactivate).catch(() => {}));
  Promise.race([Promise.all(all), new Promise((r) => setTimeout(r, 5000))]).then(() => process.exit(0));
});
// Closes the host when vsn is gone, from a thread of its own, as an
// extension may keep this one too busy to see stdin close.
new (require('worker_threads').Worker)(`
  const { ppid, pid } = require('worker_threads').workerData;
  setInterval(() => { try { process.kill(ppid, 0); } catch { process.kill(pid); } }, 2000);
`, { eval: true, workerData: { ppid: process.ppid, pid: process.pid } }).unref();
