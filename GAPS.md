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
- [ ] Most title actions are inert (Customize Layout, Split Editor, More
      Actions, Maximize Panel Size, the side bars' Views and More Actions).
- [ ] Activity bar tooltips show under the mouse; VS Code puts them to the
      right of the bar with a pointer.
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
- [ ] Change Language Mode lacks VS Code's "Auto Detect" and "Configure
      File Association" rows.
- [x] Undoing back to the saved text leaves the tab dirty (VS Code clears
      the dot).
- [ ] A file changed on disk while open isn't noticed (VS Code reloads a
      clean editor, and asks before overwriting a dirty one on save).
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
- [ ] The breadcrumb can't be scrolled back by hand (VS Code shows a thin
      scrollbar on hover), and its items aren't clickable pickers.
- [x] Go menu: Back/Forward (Alt+Left/Right, the title bar arrows) and
      Last Edit Location (Ctrl+K Ctrl+Q) were placeholders.
- [ ] Go to Symbol in Workspace (Ctrl+T) is still a placeholder.
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
- [ ] Notifications: several toasts stacked (only one warning exists so far).
