// vsn's extension host: runs JS extensions against a `vscode` module shim and
// talks to vsn in newline-delimited JSON on stdin/stdout. Plain Node APIs
// only, so Bun and Node both run it. Extensions' console output goes to
// stderr, which vsn logs.
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

// ---- state from vsn ----------------------------------------------------------

let settings = {};
let defaults = {};
let root = null;
const extensions = new Map(); // id -> { id, dir, manifest, exports, active }
const commands = new Map();

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
    if (id === 'setContext') return undefined; // ponytail: when clauses aren't evaluated
    const fn = commands.get(id);
    if (fn) return fn(...args);
    send({ m: 'execute', command: id, args });
    return undefined;
  },
  async getCommands() { return [...commands.keys()]; },
};

function message(severity) {
  // ponytail: the buttons aren't shown, so it resolves to undefined (dismissed).
  return async (text) => { send({ m: 'message', severity, text: String(text) }); return undefined; };
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
  // ponytail: to vsn's log; the Output panel when it lists channels.
  return {
    name,
    append: (s) => log(`[${name}] ${s}`),
    appendLine: (s) => log(`[${name}] ${s}`),
    replace: () => {}, clear: () => {}, show: () => {}, hide: () => {}, dispose: () => {},
    trace: (s) => log(`[${name}] ${s}`), debug: (s) => log(`[${name}] ${s}`), info: (s) => log(`[${name}] ${s}`),
    warn: (s) => log(`[${name}] ${s}`), error: (s) => log(`[${name}] ${s}`),
    logLevel: 3, onDidChangeLogLevel: never,
  };
}

const vscodeWindow = {
  showInformationMessage: message('info'),
  showWarningMessage: message('warning'),
  showErrorMessage: message('error'),
  // ponytail: no pickers or input boxes for extensions yet; they read as cancelled.
  showQuickPick: async () => undefined,
  showInputBox: async () => undefined,
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
    },
  };
}

const vscodeWorkspace = {
  getConfiguration: configuration,
  get workspaceFolders() { return root ? [{ uri: Uri.file(root), name: path.basename(root), index: 0 }] : undefined; },
  get rootPath() { return root || undefined; },
  get name() { return root ? path.basename(root) : undefined; },
  getWorkspaceFolder(uri) { return this.workspaceFolders && uri.fsPath.toLowerCase().startsWith(root.toLowerCase()) ? this.workspaceFolders[0] : undefined; },
  asRelativePath(p) { const f = typeof p === 'string' ? p : p.fsPath; return root ? path.relative(root, f).replace(/\\/g, '/') : f; },
  textDocuments: [],
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

// The API members vsn has; any other namespace answers with no-op stubs so
// an extension that touches it still activates, and any other class name
// with an empty class it can extend.
const stubs = (ns) => new Proxy({}, {
  get(_, k) {
    if (typeof k !== 'string') return undefined;
    if (k.startsWith('onDid') || k.startsWith('onWill')) return never;
    return (..._args) => {
      log(`vscode.${ns}.${k} is not implemented`);
      return new Disposable(() => {});
    };
  },
});
const known = {
  version: '1.95.0',
  // TypeScript's `import * as vscode` then uses the module as is.
  __esModule: true,
  commands: vscodeCommands, window: vscodeWindow, workspace: vscodeWorkspace, extensions: vscodeExtensions, env,
  Disposable, EventEmitter, Uri, Position, Range, Selection, ThemeColor, ThemeIcon, CancellationTokenSource,
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

function memento() {
  const m = new Map();
  return { get: (k, d) => (m.has(k) ? m.get(k) : d), update: async (k, v) => { m.set(k, v); }, keys: () => [...m.keys()], setKeysForSync() {} };
}

async function activate(id) {
  const e = extensions.get(id);
  if (!e || e.active) return;
  const storage = path.join(os.homedir(), '.vsn', 'globalStorage', id);
  const context = {
    subscriptions: [],
    extensionPath: e.dir,
    extensionUri: Uri.file(e.dir),
    asAbsolutePath: (rel) => path.join(e.dir, rel),
    // ponytail: in memory, so state is lost when vsn closes.
    globalState: memento(),
    workspaceState: memento(),
    secrets: { get: async () => undefined, store: async () => {}, delete: async () => {}, onDidChange: never },
    storagePath: storage, globalStoragePath: storage, logPath: storage,
    storageUri: Uri.file(storage), globalStorageUri: Uri.file(storage), logUri: Uri.file(storage),
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
    await fn(...(args || []));
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
  queue = queue.then(() => {
    switch (msg.m) {
      case 'init':
        settings = msg.settings || {};
        defaults = msg.defaults || {};
        root = msg.root || null;
        for (const e of msg.extensions || []) extensions.set(e.id, { ...e, active: false });
        return;
      case 'settings':
        settings = msg.settings || {};
        return;
      case 'activate':
        return activate(msg.id);
      case 'run':
        return run(msg.command, msg.args);
    }
  });
});
rl.on('close', () => process.exit(0));
