// vsn's extension host: runs JS extensions against a `vscode` module shim and
// talks to vsn in newline-delimited JSON on stdin/stdout. Plain Node APIs
// only, so Bun and Node both run it. Extensions' console output goes to
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
  registerTextEditorCommand(id) {
    // ponytail: no text editors reach the host yet, so these never run.
    return new Disposable(() => commands.delete(id));
  },
  async executeCommand(id, ...args) {
    if (id === 'setContext') {
      send({ m: 'setContext', key: args[0], value: args[1] ?? null });
      return undefined;
    }
    const fn = commands.get(id);
    if (fn) return fn(...args);
    send({ m: 'execute', command: id, args });
    return undefined;
  },
  async getCommands() { return [...commands.keys()]; },
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
  showInformationMessage: message('info'),
  showWarningMessage: message('warning'),
  showErrorMessage: message('error'),
  showQuickPick,
  showInputBox,
  showOpenDialog: async () => undefined,
  showSaveDialog: async () => undefined,
  createStatusBarItem,
  createOutputChannel,
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
  return {
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

const vscodeWorkspace = {
  getConfiguration: configuration,
  get workspaceFolders() { return folders; },
  get rootPath() { return root || undefined; },
  get name() { return root ? path.basename(root) : undefined; },
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
  fs: {
    readFile: async (uri) => new Uint8Array(await fs.promises.readFile(uri.fsPath)),
    writeFile: async (uri, data) => fs.promises.writeFile(uri.fsPath, data),
    stat: async (uri) => { const s = await fs.promises.stat(uri.fsPath); return { type: s.isDirectory() ? 2 : 1, ctime: s.ctimeMs, mtime: s.mtimeMs, size: s.size }; },
    readDirectory: async (uri) => (await fs.promises.readdir(uri.fsPath, { withFileTypes: true })).map((d) => [d.name, d.isDirectory() ? 2 : 1]),
    createDirectory: async (uri) => fs.promises.mkdir(uri.fsPath, { recursive: true }),
    delete: async (uri, o) => fs.promises.rm(uri.fsPath, { recursive: !!(o && o.recursive), force: true }),
  },
};

const vscodeExtensions = {
  getExtension(id) {
    const e = extensions.get(String(id).toLowerCase());
    return e && { id: e.id, extensionPath: e.dir, extensionUri: Uri.file(e.dir), packageJSON: e.manifest, isActive: e.active, exports: e.exports, activate: async () => e.exports };
  },
  get all() { return [...extensions.keys()].map((id) => vscodeExtensions.getExtension(id)); },
  onDidChange: never,
};

const env = {
  appName: 'Visual Studio Native', appRoot: path.dirname(process.execPath), appHost: 'desktop',
  language: 'en', machineId: os.hostname(), sessionId: String(process.pid), uriScheme: 'vsn',
  shell: process.env.ComSpec || '/bin/sh', uiKind: 1, remoteName: undefined, isTelemetryEnabled: false,
  onDidChangeTelemetryEnabled: never,
  // ponytail: no clipboard or browser reach from the host yet.
  clipboard: { readText: async () => '', writeText: async () => {} },
  openExternal: async () => false,
  asExternalUri: async (u) => u,
};

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
    // Iterates as empty, so `for (const g of window.tabGroups.all)` runs.
    if (k === Symbol.iterator) return function* () {};
    if (typeof k !== 'string' || k === 'then') return undefined;
    if (k.startsWith('onDid') || k.startsWith('onWill')) return never;
    if (k === 'dispose') return () => {};
    return stubs(`${name}.${k}`);
  },
  apply() {
    if (!unimplemented.has(name)) log(`vscode.${name} is not implemented`);
    unimplemented.add(name);
    return stubs(`${name}()`);
  },
});
// A namespace whose missing members are stubs.
const withStubs = (ns, obj) => new Proxy(obj, {
  get(t, k, r) { return k in t || typeof k !== 'string' || k === 'then' ? Reflect.get(t, k, r) : stubs(ns)[k]; },
});
const known = {
  version: '1.95.0',
  // TypeScript's `import * as vscode` then uses the module as is.
  __esModule: true,
  commands: vscodeCommands, extensions: vscodeExtensions, env,
  window: withStubs('window', vscodeWindow), workspace: withStubs('workspace', vscodeWorkspace), languages: withStubs('languages', vscodeLanguages),
  Disposable, EventEmitter, Uri, Position, Range, Selection, ThemeColor, ThemeIcon, CancellationTokenSource,
  Location, Diagnostic, DiagnosticRelatedInformation, MarkdownString, Hover, SnippetString, CompletionItem, CompletionList, TextEdit, CodeActionKind,
  WorkspaceEdit, CodeAction, CodeActionTriggerKind: { Invoke: 1, Automatic: 2 },
  SignatureHelp, SignatureInformation, ParameterInformation, SignatureHelpTriggerKind: { Invoke: 1, TriggerCharacter: 2, ContentChange: 3 },
  DiagnosticTag: { Unnecessary: 1, Deprecated: 2 },
  CompletionItemKind: Object.fromEntries('Text Method Function Constructor Field Variable Class Interface Module Property Unit Value Enum Keyword Snippet Color File Reference Folder EnumMember Constant Struct Event Operator TypeParameter User Issue'.split(' ').map((k, i) => [k, i])),
  CompletionTriggerKind: { Invoke: 0, TriggerCharacter: 1, TriggerForIncompleteCompletions: 2 },
  TextDocumentSaveReason: { Manual: 1, AfterDelay: 2, FocusOut: 3 },
  StatusBarAlignment: { Left: 1, Right: 2 },
  ConfigurationTarget: { Global: 1, Workspace: 2, WorkspaceFolder: 3 },
  ViewColumn: { Active: -1, Beside: -2, One: 1, Two: 2, Three: 3 },
  DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
  ExtensionMode: { Production: 1, Development: 2, Test: 3 },
  ProgressLocation: { SourceControl: 1, Window: 10, Notification: 15 },
  FileType: { Unknown: 0, File: 1, Directory: 2, SymbolicLink: 64 },
  EndOfLine: { LF: 1, CRLF: 2 },
  TreeItemCollapsibleState: { None: 0, Collapsed: 1, Expanded: 2 },
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
// to this one (Bun ignores `Module._load` hooks; both honor `_resolveFilename`).
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
  if (!e || e.active) return;
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
  e.active = true;
  try {
    const main = require(path.resolve(e.dir, e.manifest.main));
    e.exports = main.activate ? await main.activate(context) : undefined;
    send({ m: 'activated', id });
  } catch (err) {
    send({ m: 'error', text: `Activating extension '${id}' failed: ${errorText(err)}` });
  }
}

async function run(command, args) {
  const fn = commands.get(command);
  if (!fn) return send({ m: 'error', text: `command '${command}' not found` });
  try {
    // A menu passes its file as VS Code does, a Uri.
    await fn(...(args || []).map((a) => (a && a.$uri ? Uri.parse(a.$uri) : a)));
  } catch (err) {
    send({ m: 'error', text: `Running the contributed command: '${command}' failed. ${errorText(err)}` });
  }
}

// Requests run in order, so a command waits for its extension's activation.
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
        folders = root ? [{ uri: Uri.file(root), name: path.basename(root), index: 0 }] : undefined;
        for (const e of msg.extensions || []) extensions.set(e.id, { ...e, active: false });
        return;
      case 'settings':
        settings = msg.settings || {};
        return;
      case 'activate':
        return activate(msg.id);
      case 'run':
        // Not awaited: a command waiting on a quick pick mustn't hold up the rest.
        run(msg.command, msg.args);
        return;
      case 'lsp':
        return onLsp(msg.msg);
      case 'activeEditor':
        return setActiveEditor(msg.editor);
    }
  });
});
rl.on('close', () => process.exit(0));
