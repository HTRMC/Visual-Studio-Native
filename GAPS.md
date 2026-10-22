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
- [ ] Tooltips on the activity bar and the title-bar/panel action buttons.
- [x] Clicking Ln/Col opens Go to Line; so do Ctrl+G and the Go menu
      (Ctrl+Shift+O and the menu open Go to Symbol in Editor). In the
      terminal both keys still reach the shell, as in VS Code.
- [ ] Clicking Spaces/encoding/EOL/language opens their pickers.
- [ ] Go menu: Back/Forward (Alt+Left/Right), Last Edit Location and
      Go to Symbol in Workspace (Ctrl+T) are still placeholders.
- [ ] Do Not Disturb (the center's bell-slash button).
- [ ] Notifications: several toasts stacked (only one warning exists so far).
