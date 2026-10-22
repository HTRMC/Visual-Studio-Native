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
- [ ] Change Icon's picker is a grid of icons, not a list of names.
- [x] Terminal: Kill the Active Terminal Instance in the palette.
- [x] Terminal: Focus Next/Previous Terminal Group (Ctrl+PageDown/Up) and
      Terminal in Terminal Group.
- [x] Entering a terminal group returns to the terminal last focused in it.
- [ ] Terminal: Clear (Ctrl+K in VS Code) empties the buffer and scrollback
      (needs OpenConsole's conpty.dll for ClearPseudoConsole; clearing only
      our side would desync conhost's cursor).
- [x] Drag tabs to reorder terminals, or whole split groups.
- [x] Drag a tab onto a terminal to move it into that split group.
- [ ] Dragging a tab onto the editor area moves the terminal into an editor.
- [ ] Shell integration: command decorations, run recent command, go to
      recent directory.
