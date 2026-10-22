# Gaps

Differences from VS Code still to close, smallest first within each area.
`ponytail:` comments in `src/` hold the code-level ceilings.

## Terminal

- [x] Select All covers the scrollback, not only the visible screen.
- [x] Double-click selects a word, triple-click a line.
- [x] Dragging after a double or triple click extends by whole words or lines.
- [x] Shift+click extends the selection.
- [x] Alt+drag selects a column block.
- [x] Alt+click moves the prompt cursor to the clicked cell.
- [x] Alt+click in full-screen programs (vim, less) moves the cursor up and
      down too.
- [x] Dragging past the top or bottom edge scrolls while selecting.
- [x] A drag released outside the window keeps scrolling until the mouse
      comes back (gpui captures the mouse, so the release still arrives).
- [ ] Right-click copies the selection or pastes, without a menu
      (`terminal.integrated.rightClickBehavior` is "copyPaste" on Windows).
- [x] Links wrapped onto the next row are cut at the row end.
- [x] A wrapped link or triple-clicked line half scrolled off the top or
      bottom is cut at the screen edge.
- [x] Double-clicking a word wrapped onto the next row selects only one
      row's part.
- [ ] Wheel scrollback is lost when a shell sets application cursor mode
      (needs OpenConsole's conpty.dll).
- [x] Find in terminal (Ctrl+F).
- [x] Ctrl+F with find already open selects the query, so typing replaces it.
- [x] Find's case, whole word and regex toggles (Alt+C/W/R).
- [x] An invalid find regex reddens the box instead of quietly finding
      nothing.
- [x] Ctrl+F starts find with the terminal's one-line selection.
- [x] Find re-searches output that arrives while it's open.
- [x] Switching terminals with find open searches the newly active one.
- [x] Rename a terminal (Terminal: Rename..., or Rename... on a tab).
- [x] Rename's input starts with the old name selected, so typing replaces it.
- [x] With one terminal, the panel's title bar shows its icon and name.
- [x] Rename... from a tab's menu also starts with the name selected.
- [x] Clicking the single terminal's title-bar tab focuses the terminal.
- [x] Change a terminal's color (Terminal: Change Color..., or on a tab).
- [x] Change a terminal's icon (Terminal: Change Icon..., or on a tab).
- [x] Change Icon's picker is a grid of icons, not a list of names.
- [x] Change Icon's box is VS Code's: 486x260, 36px cells, "Search icons",
      the name's matched letters highlighted.
- [x] Terminal: Kill the Active Terminal Instance in the palette.
- [x] Terminal: Focus Next/Previous Terminal Group (Ctrl+PageDown/Up) and
      Terminal in Terminal Group.
- [x] Entering a terminal group returns to the terminal last focused in it.
- [ ] Terminal: Clear (Ctrl+K in VS Code) empties the buffer and scrollback
      (needs OpenConsole's conpty.dll for ClearPseudoConsole; clearing only
      our side would desync conhost's cursor).
- [x] Drag tabs to reorder terminals, or whole split groups.
- [x] Drag a tab onto a terminal to move it into that split group.
- [x] Unsplit Terminal (a split terminal's tab menu, and the palette).
- [x] Terminal: Join Terminals merges the selected tabs into one split group
      (Ctrl/Shift+click selects several tabs).
- [x] A terminal shown at a new size redraws at it, not only on its next
      output.
- [x] Terminal: Join Terminals from the palette: a pick of the terminals
      alone in their group, joined to the active one's.
- [x] With nothing to join, Join Terminals warns in a notification (VS Code's
      "All terminals are joined already") instead of an empty pick list.
- [x] Kill Terminal and Change Color/Icon on a multi-selection act on every
      selected tab.
- [x] Rename... on a multi-selection renames every selected tab (VS Code
      asks once and applies the name to all).
- [x] Terminal: Toggle Size to Content Width (Alt+Z) stops wrapping at the
      panel's width; Shift+wheel scrolls sideways.
- [x] The sideways scrollbar's thumb can be dragged, and shows while the
      mouse is over the terminal.
- [x] Clicking the sideways scrollbar's track jumps the thumb there and
      drags it, as VS Code's scrollbars do by default.
- [x] A vertical scrollbar over the scrollback (drag, track click, shown
      while the mouse is over the terminal).
- [x] The scrollbars fade in and out as VS Code's do.
- [x] Scrolling (typing back to the bottom, find's jumps, a drag's
      autoscroll) reveals the scrollbars briefly with the mouse elsewhere.
- [x] Keyboard scrolling: Terminal: Scroll Up/Down (Line) Ctrl+Alt+PageUp/
      PageDown, (Page) Shift+PageUp/PageDown, Scroll to Top/Bottom
      Ctrl+Home/End, outside full-screen programs.
- [x] Find's matches are marked down the right edge (VS Code's overview
      ruler), the current one gray.
- [x] A selection or find's matches stay on their text once the full
      scrollback drops its oldest lines.
- [x] Find searches only the new output, not the whole scrollback, per
      chunk.
- [x] Terminal: Find Next/Previous (F3/Shift+F3) from the terminal or the
      palette, not only Enter in find's box.
- [x] Dragging one of several selected tabs moves them all.
- [x] Dropping several selected tabs onto a terminal moves them all into
      its split group.
- [ ] Dragging a tab onto the editor area moves the terminal into an editor.
- [ ] Shell integration: command decorations, run recent command, go to
      recent directory.

## Workbench

- [x] A toast stays while the mouse is over it.
- [x] The bell opens the notification center (clear one, Clear All, hide;
      the bell dots while some are unseen; a repeat replaces the old one).
- [x] Escape closes the notification center and hides a toast, even from
      the terminal (VS Code keeps both from the shell).
- [x] Notifications: Show, Hide and Clear All Notifications in the palette.
- [ ] Notifications: Focus Notification Toast, and arrow keys through the
      center's list.
- [x] The bell's tooltip ("No Notifications", "1 New Notification").
- [x] Tooltips on the status bar items.
- [x] Tooltips on the activity bar ("Explorer (Ctrl+Shift+E)", ...).
- [x] Tooltips on the title-bar, side bar, editor and panel action buttons.
- [x] Tooltips on the command center, terminal tabs' trash, terminal find
      and the debug toolbar (Windows shows the window controls' own).
- [x] The window's taskbar title stays fixed; VS Code's is "{file} - {folder}
      - Visual Studio Code", with a dot while dirty.
- [x] Maximize/Restore Panel Size (the panel's chevron, View: Toggle
      Maximized Panel) fills the editor's space with the panel.
- [x] The editor's More Actions menu: Close All (Ctrl+K W, one prompt for
      every dirty file) and Close Saved (Ctrl+K U), also in the palette.
- [ ] More Actions lacks Show Opened Editors, Enable Preview Editors, Lock
      Group and Configure Editors (no preview editors or groups yet).
- [ ] Most title actions are inert (Customize Layout, Split Editor, the side
      bars' Views and More Actions).
- [x] Tabs have a right-click menu: Close, Close Others, Close to the
      Right, Close Saved, Close All, Copy (Relative) Path, Reveal in File
      Explorer; each is in the palette too.
- [x] The tab menu's Reveal in Explorer View (File: Reveal Active File in
      Explorer View) opens the file's folders and scrolls its row in.
- [ ] The tab menu lacks Keep Open/Pin, Split and Share.
- [x] The explorer follows the active editor (VS Code's
      `explorer.autoReveal`): each switch opens its folders and scrolls it in.
- [x] There's no `explorer.autoReveal` setting to turn following off (no
      general settings.json yet; only the terminal reads one).
- [x] settings.json with comments or trailing commas (JSONC) was ignored, and
      picking a default terminal profile then overwrote it.
- [ ] Settings are read from settings.json as each feature needs them; there's
      no file watcher and no workspace `.vscode/settings.json`.
- [x] Setting the default terminal profile rewrites settings.json as plain
      JSON, dropping its comments (VS Code edits the one property in place).
- [x] Writing the default terminal profile into a settings.json that doesn't
      parse failed silently (VS Code: "Unable to write into user settings...").
- [x] That notification lacked VS Code's "Open Settings" button; it now has
      one, which closes the notification and opens settings.json.
- [x] It was a warning; it's now an error, as in VS Code (red icon, and it
      toasts through Do Not Disturb).
- [x] Explorer failures (Unable to copy/move/create, paste into itself) were
      warnings; they're now errors, as in VS Code.
- [ ] Notifications have no info severity (nothing reports one yet); the
      "Source:" line is for extensions only.
- [x] Join Terminals with a single terminal said "All terminals are joined
      already"; it now says VS Code's "Insufficient terminals for the join
      action".
- [x] Notification buttons used gpui-component's primary colour; they now
      use button.background (#0078D4) and button.hoverBackground.
- [ ] Notification buttons aren't in the keyboard focus order (VS Code tabs
      into them from a focused notification).
- [x] A long notification ran past the toast's edge, pushing its close
      button out of view; it now wraps.
- [x] A notification's icon and close button centred on a wrapped message;
      they now stay on its first line, as VS Code's do.
- [ ] VS Code collapses a long notification to one line behind a details
      chevron (expand/collapse); ours always shows it whole.
- [ ] A settings.json that doesn't parse is ignored whole; VS Code's lenient
      JSONC parser keeps the values it can read and underlines the errors.
- [x] The explorer draws indent guides while the mouse is over the tree.
- [x] The selection's guide (its folder's, or an open folder's own) is lit
      and stays shown; the others are dimmer and show on hover.
- [x] The explorer's selected row hugged its name instead of spanning the
      side bar's width (long names now end in an ellipsis too).
- [x] Clicking a folder selects it; the selection goes back to the file on
      the next editor switch.
- [x] The explorer takes keyboard focus on a click: Up/Down/Home/End move
      the selection, Left/Right shut/open folders or step out/in, Enter
      opens into the editor, Space opens keeping focus; the selection's
      outline shows only while the explorer has focus.
- [x] PageUp/PageDown move the explorer's selection a page of rows.
- [x] PageUp/PageDown first stop at the first/last row in view, as VS
      Code's do, then move a page.
- [ ] The explorer lacks type-to-find, F2/Delete on the selection, and
      Ctrl/Shift multi-select.
- [x] Ctrl+Shift+E (View: Show Explorer, View > Explorer) opens the
      explorer and puts focus in it, instead of toggling the side bar.
- [x] The explorer's files and folders have a right-click menu: Open,
      Reveal in File Explorer, Open in Integrated Terminal (a new shell in
      that folder), Copy Path, Copy Relative Path.
- [x] The explorer header's Refresh and Collapse Folders buttons were inert
      and tipless (now also File: Refresh Explorer and View: Collapse
      Folders in Explorer in the palette).
- [ ] The explorer header's New File... and New Folder... are still inert
      (need an inline name box in the tree).
- [ ] The explorer menu lacks New File/Folder, Open to the Side, Rename and
      Delete.
- [x] The right-clicked explorer row gets the focus outline (the selection
      stays put), and the keys then move on from it, as in VS Code.
- [ ] A gpui tooltip stays on screen when its button moves away under a
      still mouse (e.g. after Restore Panel Size) until the mouse moves.
- [x] Activity bar tooltips show under the mouse; VS Code puts them to the
      right of the bar with a pointer.
- [ ] Other tooltips (status bar, title bar, action buttons) still follow the
      mouse; VS Code centres them on the item, above or below, with a pointer.
- [x] Clicking Ln/Col opens Go to Line; so do Ctrl+G and the Go menu
      (Ctrl+Shift+O and the menu open Go to Symbol in Editor). In the
      terminal both keys still reach the shell, as in VS Code.
- [x] Clicking LF/CRLF (or Change End of Line Sequence) picks the line
      endings; undo brings the old ones back.
- [x] Clicking the language (or Change Language Mode) picks the editor's
      language.
- [x] Indentation detected per file on open (`editor.detectIndentation`): status bar reads "Spaces: N" or "Tab Size: N".
- [x] Clicking Spaces opens Select Action (Indent Using Spaces/Tabs, Change Tab Display Size, Detect Indentation, Convert Indentation, Trim Trailing Whitespace) and Select Tab Size for Current File.
- [x] Clicking the encoding (or Change File Encoding) opens Reopen/Save with Encoding; Save As keeps the file's encoding.
- [ ] Reopen/Save with Encoding offer only UTF-8, UTF-8 with BOM and UTF-16 LE/BE; VS Code's other ~45 (Windows 1252, Shift JIS, ...) need encoding_rs, a new dependency.
- [ ] No `files.encoding` / `files.autoGuessEncoding` settings: new files are always UTF-8 and a BOM-less file is only told apart as UTF-8 or UTF-16.
- [x] Indentation actions are in the command palette; Trim Trailing Whitespace is Ctrl+K Ctrl+X.
- [ ] Reindent Lines / Reindent Selected Lines (need the language's indent rules).
- [ ] Changing indentation rebuilds the editor, dropping undo history (Convert/Trim can't be undone).
- [ ] Tab characters render zero-width: gpui-base shapes `\t` as text instead of advancing to the next tab stop.
- [x] Status bar Col and Go to Line columns use the file's detected tab size.
- [ ] Change Language Mode rebuilds the editor (gpui-base's set_highlighter
      doesn't re-highlight a shown one), so undo history is lost.
- [x] Change Language Mode's "Configure File Association for '.ext'..." row
      writes `files.associations` and re-languages the editor.
- [ ] Change Language Mode lacks VS Code's "Auto Detect" row (needs language
      detection) and "Configure 'X' language based settings..." (no `[lang]`
      settings here).
- [x] Change Language Mode's rows show each language's Seti icon.
- [ ] Change Language Mode's list is the 34 built-in ids; VS Code lists every
      registered language (~70 built in, plus extensions).
- [x] `files.associations` (glob → language id) picks an opened file's language.
- [ ] `files.associations` path globs match relative to the workspace root
      (VS Code: absolute path), and file icons still follow the file name.
- [x] Changing `files.associations` (saving settings.json, or Configure File
      Association) re-languages open editors, except hand-picked ones.
- [ ] settings.json changed outside vsn isn't noticed until a feature
      re-reads it (no settings watcher), so open editors keep their language.
- [x] Undoing back to the saved text leaves the tab dirty (VS Code clears
      the dot).
- [ ] A file changed on disk while open isn't noticed (VS Code reloads a
      clean editor, and asks before overwriting a dirty one on save). Seen
      with an open settings.json after Configure File Association writes it.
- [ ] No hot exit: quitting with unsaved changes asks Save / Don't Save
      (VS Code's files.hotExit "onExit" keeps them for the next start).
- [x] No "File: Revert File" command (the File menu's entry was inert).
- [x] Revert File leaves an Untitled editor as is (VS Code empties it).
- [x] File > Save As... was inert; it, Ctrl+Shift+S and the palette's
      "File: Save As..." save the active editor under a new name.
- [x] Save As onto a file open in another tab leaves both tabs (VS Code
      replaces that one).
- [ ] File > New Window is inert (one window per process).
- [x] No installer; installer\build.ps1 builds an Inno Setup one after VS Code's
      User Setup (Start menu, PATH via bin\vsn.cmd, Explorer menus, uninstall)
      and runtime\node.exe for extensions.
- [ ] The installer has no auto-update, file associations or signed exe, and
      a running vsn is only closed by Inno's Restart Manager prompt.
- [x] File > Close Window was inert; it, Ctrl+Shift+W and Alt+F4 close
      the window, asking to save dirty files first.
- [x] View > Problems, Output and Debug Console were inert; they and
      Ctrl+Shift+M/U/Y toggle the panel on that tab.
- [x] Editors always soft-wrapped and View > Word Wrap was inert; they now
      follow editor.wordWrap (default "off"), and Word Wrap / Alt+Z toggle it.
- [ ] editor.wordWrap "wordWrapColumn"/"bounded" wrap at the window edge,
      not editor.wordWrapColumn.
- [x] Alt+Z toggled every editor; like VS Code it now flips the active one
      only, and View > Word Wrap's check follows the active editor.
- [x] With no editor open, View > Word Wrap was still enabled; it's greyed
      out as in VS Code.
- [ ] Still-inert menu entries: Open Recent > More..., Preferences >
      Keyboard Shortcuts / Themes; View > Editor Layout > Split Up/Down;
      Run > Add Configuration..., Run Task..., Run Build Task...;
      Help's Documentation, Release Notes, Keyboard Shortcuts Reference,
      Report Issue (VS Code opens web pages), View License, Toggle Developer
      Tools.
- [x] File > Open Recent held only More...; it lists recent folders (not
      the open one) and files opened from outside the explorer, as VS Code.
- [ ] Open Recent lacks Reopen Closed Editor, and leaves out gone paths
      where VS Code offers to remove them.
- [x] Open Recent > Clear Recently Opened... (and the palette's) clears it
      and Quick Open's history, behind VS Code's warning.
- [x] Run > Open Configurations was inert; it and "Debug: Open 'launch.json'"
      open launch.json, creating it first, like the Run view's gear.
- [x] Help > About (and Help: About) showed nothing; it's VS Code's version
      box with Copy.
- [ ] The About box's title bar reads "Info" (gpui titles prompts by level;
      VS Code's reads its name).
- [x] The About box's OS line lacks the Windows version (now RtlGetVersion's
      "10.0.26200", as VS Code shows it).
- [x] Edit and Selection menu entries (Undo, Cut, Select All...) never
      reached the editor and showed no shortcuts; they now go to whatever
      had focus when the menu opened. Selection > Add Cursor Above/Below
      work.
- [x] Selection > Copy/Move Line Up/Down were inert; they and
      Shift+Alt+Up/Down, Alt+Up/Down copy or move the selected lines.
- [ ] Copy/Move Line take the active selection's lines only, not every
      cursor's; undoing one leaves the swapped lines selected instead of
      putting the cursor back.
- [x] Edit > Toggle Line Comment was inert; it and Ctrl+/ comment or
      uncomment the selected lines with the language's line comment.
- [x] Edit > Toggle Block Comment was inert; it and Shift+Alt+A wrap or
      unwrap the selection, and Toggle Line Comment block-comments the
      lines in CSS/HTML/XML/Markdown.
- [x] Ctrl+K Ctrl+C / Ctrl+K Ctrl+U (Add / Remove Line Comment) were
      missing; they and the comment commands are in the palette too.
- [ ] Toggle Line/Block Comment handle the active selection only.
- [x] The palette had no Copy/Move Line Up/Down, Add Cursor Above/Below
      or Undo/Redo commands, which VS Code lists.
- [x] Ctrl+D (Add Next Occurrence) and Ctrl+Shift+L (Select All
      Occurrences) were missing; they're in the Selection menu and the
      palette, and the palette has Cut/Copy/Paste and Select All.
- [x] Ctrl+D matched case-sensitive substrings; now a word it picked from
      the cursor matches whole words, case-sensitively, and a selection
      matches anywhere in any case, as in VS Code.
- [x] Ctrl+D folded ASCII case only; "été" now finds "Été" and "ÉTÉ" too.
- [x] Ctrl+D ignored the open find widget's Match Case; it now applies.
- [ ] The editor's find widget lacks Whole Word and Regex toggles (gpui-base's
      panel has Match Case only), so Ctrl+D can't follow them either.
- [x] Ctrl+K Ctrl+D (Move Last Selection to Next Find Match) was missing;
      it's in the palette too, and Ctrl+D skips matches already selected.
- [ ] The occurrence commands only know the selections they added; ones
      from Alt+Click or Add Cursor aren't skipped or moved (gpui-base has no
      way to read an editor's other selections).
- [x] Selection > Add Previous Occurrence was missing; it's in the menu
      and the palette (Add Selection To Previous Find Match).
- [x] Selection > Expand/Shrink Selection were inert; they and
      Shift+Alt+Right/Left step out through the word, brackets, line and
      file, and back.
- [ ] Expand Selection uses brackets, not the syntax tree, so strings,
      comments and brackets inside them don't count; it handles the
      active selection only.
- [x] View > Open View... was inert; it and Ctrl+Q list the side bar and
      panel views in the palette (`view ` prefix) and show the picked one.
- [x] Open View lists each open terminal under "Terminal", numbered
      "group.index: name" as VS Code does, and picking one focuses it.
- [ ] Open View lists whole containers, not their sub-views (Open Editors,
      Outline, Timeline), and has no Output-channel rows.
- [x] Open View filtered on labels only; like VS Code it now also keeps rows
      whose group name contains the filter ("view term" lists every terminal).
- [x] File > Auto Save was inert; it and "File: Toggle Auto Save" flip
      files.autoSave, and "afterDelay" saves dirty files after
      files.autoSaveDelay.
- [ ] files.autoSave "onFocusChange" / "onWindowChange" count as off, and
      the delay is one debounce for all editors (VS Code's is per editor).
- [x] Writing a setting (default profile, file associations) rewrote
      settings.json as plain JSON, dropping its comments; it's now edited in
      place like VS Code's jsonc-parser `modify`.
- [x] Enter in a CRLF file inserts a bare LF; saving now writes the file's
      one ending throughout, and the label goes by most endings.
- [x] New untitled files start LF; VS Code's files.eol "auto" makes them
      CRLF on Windows (so do files opened without line breaks).
- [ ] Enter inserts a bare LF even in a CRLF file (gpui-base hardcodes it);
      saving fixes the bytes, but copying lines out carries mixed endings.
- [x] The breadcrumb of a file outside the folder showed a stray `\` after
      `C:`.
- [x] A breadcrumb wider than the editor runs off the right edge, hiding
      the file name; VS Code scrolls it to show the end.
- [x] The breadcrumb can be scrolled back by hand with the wheel; it
      reveals the file name only on a file switch or a width change.
- [x] The breadcrumb shows VS Code's 3px scrollbar while the mouse is over
      it (and briefly after a scroll), when the path overflows.
- [ ] The breadcrumb's items aren't clickable pickers.
- [x] The editor tab strip and the explorer tree have no scrollbars (VS Code
      shows a 3px one under the tabs and a 10px one on the tree on hover);
      many tabs widened the editor and pushed its actions off-screen.
- [x] The active tab isn't scrolled into view in the tab strip (VS Code
      reveals it on a switch).
- [x] Double-clicking the tab strip's empty space doesn't open a new
      untitled file, as VS Code's does.
- [x] Middle-clicking a tab doesn't close it, as VS Code's does.
- [x] Hovering a tab shows no tooltip with the file's full path.
- [x] A tab's close button has no "Close (Ctrl+F4)" tooltip.
- [x] A dirty tab's dot doesn't turn into the close X on hover, as VS
      Code's does.
- [x] Editor tabs can't be dragged to reorder them.
- [x] Dragging a file from the explorer onto the tab strip or editor
      doesn't open it.
- [x] Dragging an explorer file onto the terminal doesn't type its path.
- [x] Files dragged in from Windows Explorer aren't accepted (VS Code opens
      them in the editor, or types their paths into the terminal).
- [x] Files from Windows dropped on a tab open at the end, not at that tab.
- [x] A drag over a tab only tints it faintly; VS Code also draws
      `tab.dragAndDropBorder` on the side the tab will land.
- [x] Files from Windows dropped on the explorer aren't copied into the
      folder under the mouse.
- [x] A drop over the explorer tints only the row under the mouse; VS Code
      tints the whole target folder with its children.
- [x] Dragging in the explorer can't move files between folders.
- [x] Ctrl+drag in the explorer doesn't copy.
- [x] The explorer has no Cut/Copy/Paste (Ctrl+X/C/V and the row menu).
- [x] Explorer Paste is always enabled in the row menu (VS Code greys it out
      with nothing copied).
- [ ] Files copied in Windows Explorer can't be pasted into the explorer,
      nor explorer items pasted out to Windows (gpui's clipboard is text
      and images only; needs CF_HDROP).
- [x] The explorer's empty area has no right-click menu (VS Code's Paste,
      Reveal, Open in Integrated Terminal, Copy Path for the root).
- [x] Explorer menus lack Find in Folder (Shift+Alt+F; VS Code puts it on
      folder rows and the empty area).
- [x] The search's "files to include" box lacks VS Code's "Search Only in
      Open Editors" toggle.
- [x] With "Search only in Open Editors" on, opening or closing an editor
      doesn't re-run the search, and an empty result lacks VS Code's
      "No results found in open editors" wording.
- [x] The search's empty-result message lacks VS Code's trailing "Open
      Settings" and "Learn More" links.
- [x] Preferences > Settings (Ctrl+,) was inert; it and the palette's
      "Preferences: Open User Settings" now open settings.json.
- [ ] No Settings UI: Ctrl+, opens settings.json (VS Code's
      `workbench.settings.editor: "json"`), and Open Settings opens it rather
      than a Settings UI filtered to ".exclude".
- [x] The palette lacks "Preferences: Open User Settings (JSON)".
- [ ] The palette lacks "Preferences: Open Keyboard Shortcuts" and the
      other Preferences commands (no keybindings editor or keybindings.json).
- [x] The "files to exclude" box lacks VS Code's "Use Exclude Settings and
      Ignore Files" toggle (the search always honours .gitignore).
- [x] Search and the explorer ignore the `files.exclude` and `search.exclude`
      settings (VS Code hides `.git` etc. and skips `node_modules` by default).
- [x] `files.exclude` ignores `{"when": ...}` sibling conditions.
- [ ] An edit to settings.json shows in the explorer only after a refresh (no
      watcher).
- [ ] A `when` sibling is checked with a stat per matching entry on every
      explorer render and walk; no cache of the folder's names.
- [x] A right-click in the explorer's empty area doesn't move the focus
      outline off the last row the way VS Code clears it.
- [ ] The explorer handles one item at a time: no Ctrl/Shift+click
      multi-select for drag, cut, copy or paste.
- [ ] An explorer move or copy can't be undone (Ctrl+Z in the explorer), and
      the move prompt lacks VS Code's "Do not ask me again"
      (`explorer.confirmDragAndDrop`).
- [ ] Holding Ctrl mid-drag doesn't show a copy cursor ("+" badge) the way
      VS Code's drag image does.
- [x] Go menu: Back/Forward (Alt+Left/Right, the title bar arrows) and
      Last Edit Location (Ctrl+K Ctrl+Q) were placeholders.
- [x] Go to Symbol in Workspace (Ctrl+T, `#` in Quick Open, the Go menu,
      the palette) lists every workspace file's definitions by fuzzy name
      and opens the picked one.
- [ ] Workspace symbols are tree-sitter definitions parsed again on each
      Ctrl+T (no index, no progress bar while parsing; files over 1 MB
      skipped), only the best 50 show, rows lack the container name, and
      there's no preview; VS Code asks language servers.
- [x] Jumping to a line (Go to Symbol, `file:line`, search results, terminal
      links, stack frames) put it at the viewport's edge; VS Code centers it.
- [x] The mouse's back/forward side buttons didn't navigate.
- [x] Go menu: Switch Editor's Next/Previous Editor (Ctrl+PageDown/Up).
- [ ] Go menu's other navigation entries (Next/Previous Used Editor's
      Ctrl+Tab picker, Switch Group, Next/Previous Problem, Next/Previous
      Change) are missing.
- [x] Menus and the command palette showed only a chord's first key
      ("Ctrl+K" for Last Edit Location's Ctrl+K Ctrl+Q, Save All's Ctrl+K S).
- [x] Do Not Disturb (the center's bell-slash button, Notifications: Toggle Do Not Disturb Mode): the bell turns bell-slash and warnings skip the toast.
- [x] Do Not Disturb is remembered across restarts (state.json, as VS Code's application storage).
- [ ] Do Not Disturb lacks VS Code's per-source filter (needs extensions as notification sources).
- [x] Notifications: several toasts stacked (only one warning exists so far).
- [x] A 4th toast drops the oldest instead of queueing it until one closes.
- [x] A queued toast that comes into view may hide sooner than a full 18
      seconds.
- [x] Toasts appeared without VS Code's slide-in; they now fade in from below.
- [ ] Tab characters in a file render with no width (tabs.c's indents vanish).

## Extensions

See EXTENSIONS.md for the plan; phases 1–3 are in, phase 4 has diagnostics, hover, completion, Go to Definition and formatting, from built-in servers or language extensions.

- [x] Install from VSIX... (the view's ... menu, and the palette).
- [x] Installed list with search, Disable and Uninstall on the gear.
- [x] Preferences: Color Theme (Ctrl+K Ctrl+T) applies an extension's theme and saves `workbench.colorTheme`.
- [x] Snippets: Insert Snippet; languages' file extensions, labels and comments.
- [x] The terminal's text followed no theme; it takes `terminal.foreground`.
- [ ] Toasts, menus and other hard-coded `c(0x..)` colors stay dark under a light theme.
- [ ] The theme picker doesn't preview as the selection moves.
- [x] Snippets: Tab and Shift+Tab move between tab stops, inserted or completed by prefix.
- [ ] Snippets: a repeated tab stop isn't mirrored; edits away from the current stop put the later ones off.
- [ ] Extension details page (README, changelog) when clicking a row.
- [x] Open VSX search and install: typing in the Extensions view searches open-vsx.org; `@installed` filters the Installed list.
- [ ] Open VSX results have no icons, ratings or download counts, no paging past 30, no update checks, and no `@` filters besides `@installed`.
- [ ] Ctrl+Shift+X opens the Extensions view without focusing its search box.
- [x] WASM plugins (`"vsn": { "wasm": ... }`) run in wasmtime with fuel and memory limits; examples/hello-wasm is a sample.
- [x] WASM plugins got no command arguments, settings, document or questions; WIT 0.2 has them.
- [ ] A trapped WASM plugin stays unloaded until the extensions restart, and a stuck call blocks the other plugins' calls until its fuel runs out; a quick pick waits for its answer the same way.
- [x] The WIT API had no providers or document edits; WIT 0.3 has `lsp` and `apply-edit`.
- [ ] Only the first WASM plugin for a language answers its requests; VS Code merges every provider's.
- [x] The WIT API had no file or network access; `vsn.permissions` asks the user for it.
- [ ] A WASM plugin's permission answers last until vsn closes, and there's no list of them to revoke.
- [x] Brackets, auto-closing pairs and indent rules from extensions' language configurations.
- [x] Icon themes from extensions.
- [x] An installed extension's details page: header, Disable/Uninstall, Details (README), Features, Changelog.
- [ ] Open VSX results have no details page, and a README's relative images don't load.
- [ ] The details page's Features tab lists raw contribution points, not VS Code's named tables per kind.
- [ ] Icon themes' light and high contrast variants, and font (glyph) icons, are skipped; only image icons show.
- [ ] Picking "None" as the icon theme saves `""`, not `null` (settings can't hold a null yet).
- [ ] Extension indentation patterns with lookaround (JS regex) are skipped; the editor's regex has none.
- [ ] A line matching the decrease-indent pattern outdents only on Enter, not as it's typed (gpui-base).
- [x] JS extensions run in a Node (or Bun) host: commands in the palette and keys, messages, status bar items, configuration defaults, activation events, restart on crash.
- [x] Extension keybindings took effect only after a restart; they're rebound on install, uninstall and enable.
- [x] Extension keybindings with a `when` clause were skipped; focus keys, `editorLangId` and the platform are matched now.
- [x] Extension keybindings' `when` with `<`, `>`, `=~` or `in` were skipped; they're checked in full as the key comes.
- [x] `commandPalette` `when` clauses other than `false` were ignored; they're checked where focus was when the palette opened.
- [x] Extensions' `editor/context` and `explorer/context` menu items were ignored; they're added, with the file's Uri.
- [x] Extensions' `editor/title` navigation buttons were ignored; they're in the editor's title bar, with `resource*` keys for their `when`.
- [x] Extensions' `when` clauses with comparisons, regexes or `in` hid their menu and palette items, and `setContext` was ignored; menus and the palette evaluate VS Code's full grammar.
- [x] Extension keybindings' `when` with comparisons, regexes or `setContext` keys were skipped; the same check covers them.
- [x] Extensions' `editor/title` non-navigation groups and submenus were ignored; they're in "..." and flattened into "Submenu › Item".
- [x] Extensions' `editor/title/context` and `view/title` items were ignored; they're on the tab menu and the built-in views' headers.
- [x] Extensions' `scm/*` menus and source controls: the Source Control view shows their groups and resources, with title, inline and row-menu items.
- [x] Extensions' own views: activity bar containers, tree views, welcome content, `view/title` and `view/item/context` menus.
- [ ] Extension views: no panel containers or webview views; views don't resize or reorder; a change refreshes the whole tree; no `reveal`, selection, checkboxes or expand events; links inside welcome text don't click.
- [ ] Extension source controls: one commit box, the first provider's; groups don't collapse; no status letters, multi-select or `actionButton`; `resourceStates` changed in place (not reassigned) don't sync.
- [ ] Real nested submenus; with extension items, the editor's own menu items are always enabled.
- [x] Quick pick, input box and message buttons for extensions, in the palette and the notification.
- [x] `globalState`/`workspaceState` persist (JSON under `~/.vsn/globalStorage` and `workspaceStorage`); `storageUri` is per folder.
- [x] `executeCommand` runs ~35 of VS Code's built-in commands (`vscode.open`, saves, views, panels, formatting, ...) as vsn's actions.
- [x] `executeCommand` runs ~170 of VS Code's built-in commands (every one vsn has an action for: files, editors, views, panels, terminal, debugging, the editor's cursor, clipboard and line commands), `type`, `vscode.openFolder`, and `getCommands` lists them; `registerTextEditorCommand` runs with the active editor.
- [ ] Built-ins that return results (`vscode.executeHoverProvider` and friends) and ones without a vsn action log "not found"; built-ins ignore their arguments; `type` leaves the cursor before the text it inserts.
- [x] Extensions' `deactivate()` and `context.subscriptions` run when vsn closes or restarts the host (install, uninstall, enable, disable), up to 5s, under Node and Bun.
- [ ] Under Bun, `process.on('exit')` listeners don't run when vsn closes.
- [x] `window.activeTextEditor` with its document, selection, `edit()` and `insertSnippet()`; `workspace.applyEdit`.
- [ ] The active editor is sent on a tab switch and before a command: selection changes in between aren't (`onDidChangeTextEditorSelection` never fires), closing a tab doesn't send the next one, `setDecorations` draws nothing, and only one editor is visible.
- [ ] `secrets` is a stub that stores nothing; `setKeysForSync` does nothing (no settings sync).
- [ ] Extension quick picks pick one even with `canPickMany`, `createQuickPick`/`createInputBox` are stubs, input boxes don't run `validateInput`, show `prompt` only as the placeholder, and a password box reads as cancelled; `modal` messages are toasts.
- [x] Output channels, the extension host's console and language server logs
      show in the Output panel, picked from its channel dropdown.
- [ ] Output: `append` always starts a new line, `show()`/`clear()`/`replace()`
      do nothing, no filter, no word-wrap toggle, channels aren't sorted.
- [ ] VS Code's own commands (`vscode.open`, `workbench.action.*`) called by extensions do nothing.
- [ ] `globalState`/`workspaceState` are in memory only; `activeTextEditor` doesn't reach the host.
- [x] Documents reach the host (open, change, save, close).
- [x] `workspaceContains:` matches globs (`**/*.zig`) as well as paths.
- [ ] `workspaceContains:` globs walk the folder on the UI thread, capped at 10,000 entries; VS Code searches in the background.
- [x] Language servers on PATH (rust-analyzer, clangd, gopls, zls, basedpyright, typescript-language-server) give squiggles, the Problems panel and the status bar counts.
- [x] Hover, completion (as you type and Ctrl+Space) and Go to Definition (F12, Ctrl+Click, into other files) from language servers.
- [x] Format Document (Shift+Alt+F) from language servers, as one undo step.
- [x] Code actions (Ctrl+.) from language servers and extensions, applied as one undo step.
- [ ] Code actions: file operations, `workspace/applyEdit` and `codeAction/resolve` are dropped; no lightbulb.
- [x] Rename Symbol (F2) from language servers and extensions, edits across files.
- [ ] Rename asks in the palette, not an inline box; no `prepareRename`, and edited files open as tabs.
- [x] Find All References (Shift+F12) from language servers and extensions, as a pick list grouped by file.
- [ ] References show in a pick list, not a peek view or the References panel; no Go to References or Shift+Alt+F12.
- [x] Format Selection (Ctrl+K Ctrl+F; the cursor's line when nothing is selected) from language servers and extensions.
- [ ] Format on save/type and document symbols from language servers.
- [x] Parameter hints after `(` and `,` or on Ctrl+Shift+Space, from language servers and extensions.
- [ ] Parameter hints ignore the server's trigger characters, don't follow cursor moves, can't cycle overloads, and show markdown as plain text.
- [ ] Completion ignores the server's trigger characters and snippets (`snippetSupport` is off).
- [ ] Problems panel: collapsing a file, filter, related information, and the Explorer's red file decorations.
- [x] A crashed language server restarts and reopens its files, up to 5 times.
- [ ] A language server given up on leaves its squiggles behind.
- [x] Language extensions (`vscode-languageclient`, e.g. Zig's) run their own server in the host; hover, completion, definition, formatting and diagnostics reach the editor, and the built-in server for that language steps aside.
- [x] An extension's `update(key, undefined)` removes the setting instead of writing `null`.
- [ ] Host providers: document selectors' `pattern`/`scheme` aren't checked, completion snippets are flattened to plain text, and several diagnostic collections for one file overwrite each other.
- [ ] `createFileSystemWatcher` never fires; `openTextDocument` reads the disk, not the open editor.
- [ ] Only hover, completion, definition, (range) formatting, code action, rename, reference and signature help providers; the rest log "not implemented". Only the first matching provider answers.
- [ ] A language extension claiming a TypeScript-family language stops the shared built-in server for the others until their next file.
- [ ] Completion list isn't filtered by the typed prefix while it's open.
