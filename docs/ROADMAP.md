# RustCode Roadmap

Goal: a VS Code-compatible code editor written in Rust, with no Electron or bundled Chromium.
UI is built on GPUI. VS Code extensions run in a Node.js extension host that talks to the Rust
core over VS Code's own RPC protocol. Webview extensions use the system webview (`wry`), not a
bundled browser.

Status legend: **done**, **in progress**, **planned**.

## Constraints and decisions

- **Extensions are JavaScript.** They call the `vscode` API in a Node extension host. Removing
  Electron removes the UI shell, not the need for a JS runtime.
- **Reuse VS Code's extension host.** VS Code's source is MIT. Run its `extensionHost` code in
  Node and implement the "main thread" side (the `MainThread*` services) in Rust.
- **Marketplace.** Microsoft's Marketplace terms block non-Microsoft products. Use Open VSX.
- **Compatibility is never complete.** The API surface and proposed APIs keep growing. Target
  the most-used extensions, then widen.
- **Clean architecture.** Dependencies point inward: `presentation`, `infrastructure` ->
  `application` -> `domain`. The domain crate has no I/O and no UI types.

## Workspace layout

```
rustcode/
  crates/domain          entities: TextBuffer, Editor, Selection, history, themes, search
  crates/application     use cases and ports (traits implemented by infrastructure)
  crates/infrastructure  adapters: file system, TextMate highlighting, VS Code theme loader
  crates/presentation    GPUI views: editor, find bar, theme conversion
  apps/rustcode          composition root: wires adapters into use cases and starts the UI
  themes/                bundled VS Code theme files used for testing
  docs/                  this roadmap
```

## Phase 0: Spike (done)

- GPUI window rendering a virtualized text view.
- Clean-architecture workspace.
- Success: scroll a 100k-line file smoothly.

## Phase 1: Core editor (mostly done)

Done:

- Rope text buffer (`ropey`), multi-cursor selections, undo/redo with typing coalescing.
- Movement: arrows, word, line, document, with shift-extend. Goal column on vertical moves.
- Clipboard, save, dirty flag, auto-indent on newline.
- Mouse: click, shift-click, alt-click (add cursor).
- Syntax highlighting: TextMate grammars through `irosashi` (a Rust port of vscode-textmate).
  Tokenizing is incremental and time-sliced, so large edits do not block the UI.
- VS Code `.json` theme loading (JSONC, `include`, `tokenColors`, descendant selectors).
- Find and replace: case, whole word, regex with capture groups, replace all as one undo step.
  Find box supports click-to-place caret and up/down history.
- Mouse: drag selection, double-click word, triple-click line.
- Shaped text layout (wide characters, tabs) with mouse-to-column mapping.
- IME and dead-key input through the platform text input handler.
- Runtime theme cycling (`cmd-k cmd-t`) and `--theme FILE`.

- `.tmTheme` files load as themes, or as the `tokenColors` of a VS Code theme.

## Phase 2: Workbench shell (in progress)

Done:

- Command registry (`application::commands`) with fuzzy search, and a command palette
  (`cmd-shift-p`) that lists and runs the built-in commands.
- Extension host (first slice): folders of VS Code extensions (`~/.rustcode/extensions`, and
  `--extensions DIR`, repeatable) are read for `contributes.commands` (titles may use
  `package.nls.json`), and those commands show in the command palette as `Category: Title`.
  Running one starts a separate JavaScript process (`node`, else `bun`; found on `PATH`, `nvm`,
  Volta or Homebrew; `RUSTCODE_JS_RUNTIME=bun|node|/path` chooses; Bun idles at about a third of
  Node's memory and passes the same extension host tests) that loads
  `extension_host.js`, provides a small `vscode` module, activates the extension (also at
  startup for `*` and `onStartupFinished`) and calls the command. Supported: `commands`
  (`registerCommand`, `executeCommand`), `window.show{Information,Warning,Error}Message`
  (shown in the status bar; no buttons), `setStatusBarMessage`, `createOutputChannel` and
  `console.log` (to the Output tab), `workspace.workspaceFolders`, `getConfiguration` (defaults
  only), `Uri`, `Position`, `Range`, `EventEmitter`, `Disposable` and the `context` object.
  Anything else throws "vscode.x.y is not supported by RustCode yet", shown as an extension
  error. (Documents and editors came later, see Phase 4, layer 3. No tree views, webviews or
  language features yet.)
- Double-clicking the empty space after the last tab opens a new untitled file (a double click on
  a tab does not). Files dragged in from the desktop or Finder open in the editor they are dropped
  on, or in the focused one when dropped elsewhere; folders are left alone.
- Menu bar in the window on macOS: `View: Toggle Menu Bar in Window (keyboard access)` draws the
  menu row under the title bar, so mnemonics and tapping `Alt` work there too. The choice is
  saved in the session.
- Workbench colors from VS Code theme files: every key of `colors` is kept, and the interface
  reads these (each falls back to the look it had before, so a theme that omits a key changes
  nothing): `activityBar.*` (background, foreground, inactiveForeground, activeBorder,
  activeBackground, border), `sideBar.*` (background, foreground, border), `sideBarTitle.foreground`,
  `sideBarSectionHeader.*`, `list.*` (activeSelection, inactiveSelection, hover, with
  foreground), `editorGroupHeader.tabsBackground`, `tab.*` (active and inactive background and
  foreground, border, activeBorderTop or activeBorder, unfocusedActiveBorderTop),
  `statusBar.*` and `statusBarItem.hoverBackground`, `panel.*` and `panelTitle.*`,
  `editorGroup.border`, `menu.*` and `menubar.selectionBackground`, `quickInput.*` and
  `quickInputList.focusBackground` (the palette), `terminal.*` (background, foreground,
  selection, cursor and the 16 `ansi` colors), `editorError`, `editorWarning` and `editorInfo`
  foregrounds (squiggles, gutter dots, status bar, Problems), `gitDecoration.*` (explorer and
  Source Control), `editorGutter.*` (background and the added, modified, deleted marks),
  `editorLineNumber.activeForeground`, `diffEditor.*Line/TextBackground`, `merge.*`
  backgrounds, `button.*` (the Commit button) and `focusBorder`, plus these, each with the
  interface that shows it:
  - Scrollbars (`scrollbarSlider.*`): a thumb on every scrollable list (editor, explorer,
    search results, changes, Problems, Output, Graph). Drag it, or press beside it to put the
    thumb there. The editor's horizontal scroll and the terminal have none yet.
  - Badges (`activityBarBadge.*`, `badge.*`): the number of changed files on the Source Control
    icon, and the number of problems beside PROBLEMS.
  - Notifications (`notifications.*`, `notificationToast.border`, `notificationLink.foreground`,
    `notifications{Info,Warning,Error}Icon.foreground`): messages from extensions pop up at the
    bottom right (6, 10 and 15 seconds for info, warnings and errors; at most four; click the
    cross to dismiss). An error offers "Show Output".
  - Breadcrumbs (`breadcrumb.*`): the file's path above the editor, the workspace folder then
    each folder; click a part to show it in the explorer. `"breadcrumbs.enabled": false` hides
    the row. Symbols are not listed.
  - Inputs (`input.border`, `input.foreground`, `input.placeholderForeground`,
    `inputOption.activeBorder`, `inputOption.activeBackground`, `inputValidation.error*`): the
    find box shows the error colors for a pattern that does not parse.
  - Window (`titleBar.inactiveBackground`, `titleBar.inactiveForeground`, `titleBar.border`,
    `window.activeBorder`, `window.inactiveBorder`): the in-window title bar dims when the
    window is not in front; the window frame is drawn only when the theme names one.
- Settings from VS Code style `settings.json` (comments and trailing commas are fine), read from
  VS Code's user folder (`~/Library/Application Support/Code/User` on macOS, `~/.config/Code/User`
  on Linux), then `~/.rustcode/settings.json`, then the workspace's `.vscode/settings.json`;
  later files win key by key. The files are read again whenever their text changes, so an edit
  applies within a moment; a file that is not valid JSON is reported in the status bar and the
  Output tab and ignored. Used: `editor.fontSize` (8 to 48), `editor.lineHeight` (a multiple
  below 8, pixels above), `editor.tabSize`, `editor.insertSpaces`, `editor.wordWrap` (`off`
  also turns off the default wrapping of Markdown and text; a toggle by hand wins),
  `workbench.colorTheme` (VS Code labels such as "Default Light+" find the matching theme;
  applied when the setting changes, so a theme picked by hand stays), `files.autoSave`
  (`afterDelay` only) with `files.autoSaveDelay`, `files.trimTrailingWhitespace` and
  `files.insertFinalNewline` (on every save, one undo step), and `editor.fontFamily` (the first
  installed family, at start only; `--code-font` wins). `[lang]` blocks (`"[rust]": {...}`,
  `"[typescript][javascript]"`) override the plain settings for that language, and are read for the
  formatter, indentation, on-save, word wrap, inlay hint, semantic highlighting and bracket settings.
  Not used yet: `settings.json` editing in the app.
- Key bindings from VS Code style `keybindings.json`, in the same two user folders (VS Code's,
  then `~/.rustcode/`). `key` takes `ctrl+shift+p`, `cmd+k cmd+c` chords and the usual key names;
  `command` takes a VS Code command id (about 60 are mapped, such as
  `workbench.action.files.save`), a command id of this program (`view.togglePanel`, `git.stash`)
  or an extension command; a `-command` entry removes the built-in shortcut on that key; a
  later entry beats an earlier one. `when` clauses support `&&`, `||`, `!`, parentheses and
  `==` / `!=` over `editorTextFocus`, `terminalFocus`, `inputFocus`, `findWidgetVisible`,
  `editorHasSelection`, `editorLangId`, `resourceExtname`, `explorerViewletVisible`,
  `sideBarVisible`, `panelVisible`, `multipleEditorGroups`, `isMac` and a few more (unknown
  keys are false). The keys are matched before GPUI sees them, so they can replace built-in
  shortcuts; a chord that begins like a built-in one (`cmd+k`) takes it over and still reaches
  the built-in second keys. The command palette shows your shortcut. `args` are ignored.
- Go to File (`cmd-p`, `ctrl-p` elsewhere): fuzzy picker over the workspace's files, read in the
  background with `.gitignore` honored (`.git` is skipped, dotfiles are kept; at most 50,000
  files). A name match outranks a folder match; with nothing typed, open files come first.
  `name:12` or `name:12:5` opens the file at that line and column. A leading `:12` goes to a
  line of the current file. Go to Line (`ctrl-g`) takes `12` or `12:5` and keeps to the file.
  On Linux and Windows `ctrl-g` is Go to Line, so Find Next and Previous are `F3` and
  `Shift+F3` there (Cmd+G and Cmd+Shift+G on macOS).
- Theme picker (`cmd-k cmd-t`, or "Preferences: Color Theme" in the palette). It previews the
  highlighted theme, `enter` keeps it, `escape` restores the previous one.

- Tabs over several documents: new, open by path, save as, close, next/previous. Each tab keeps
  its own text, cursor, undo history, highlighting and scroll position. Closing a tab with
  unsaved changes needs a second request.
- Status bar: messages, line and column, selection size, line count, language, theme.
- Menus (File, Edit, Selection, View, Help), one definition for every platform. macOS shows them
  in the system menu bar, with an application menu (About, Quit). Windows draws them in a
  custom title bar with minimize, maximize and close buttons and a drag area. Linux draws them
  in a row under the native title bar. Items are actions, so a click behaves like the
  shortcut, and shortcuts show as `⇧⌘P` on macOS and `Ctrl+Shift+P` elsewhere.
- Menu keyboard access in the in-window menus: `Alt+F/E/S/V/H` open a menu; arrows move,
  `Right` enters a submenu, `Left` leaves it, `Enter` runs, `Escape` backs out.
  Submenus are supported (View > Editor Tabs). Every title and item has a mnemonic, written
  `&File` in the menu definition and underlined while the menu bar has the keyboard; a letter
  runs the item (or opens the submenu) at once. Tapping and releasing `Alt` alone puts the
  keyboard on the bar: the first title is highlighted, arrows move between titles, `Down` or
  `Enter` opens one, a letter opens the menu with that mnemonic, `Escape` or `Alt` again leaves.
  Alt with another key or modifier is a shortcut, not a tap. macOS uses its system menu bar.
- Key bindings use the `secondary` modifier (Cmd on macOS, Ctrl elsewhere); line and word
  movement have per-platform bindings. `--chrome windows|linux|macos` previews another
  platform's menu and title bar layout.
- Open and Save As prompts complete directory entries (Tab completes, Enter on a folder
  browses into it). Tabs scroll horizontally and keep the active tab visible.

- Activity bar (explorer and panel buttons), side bar and a bottom panel showing an output
  log (`cmd-b`, `cmd-j`).
- File explorer (`cmd-shift-e` focuses it):
  - Several workspace folders (Add Folder to Workspace, Remove Folder). The first folder is the
    argument, the folder of the argument file, or the working directory.
  - Folders load lazily, files open in the active editor group, and the active file is
    highlighted and revealed. Folders refresh when files change on disk (`notify`, one watch
    per expanded folder).
  - Create file, create folder, rename and delete (to the system trash, after a second
    request), from a right-click menu, the command palette, or `F2` / `Delete`. Open tabs
    follow renamed files and folders.
  - Keyboard: arrows move, `Right` and `Left` open and close folders, `Enter` opens,
    `Escape` returns to the editor. Editor shortcuts keep working.
  - `.gitignore` files (root and nested, with negation) dim ignored entries and everything
    inside ignored folders.
  - Colored file-type dots, and a side bar you resize by dragging its edge.
- Editor groups, laid out as a tree of splits:
  - `cmd-\` splits right, `cmd-k cmd-\` splits down. Left and up are in the palette and menu.
  - Each group has its own tab strip. Tabs can be dragged to reorder, into another group, or
    onto a group's edge to open a new group there. Dividers resize groups.
  - One document in two groups has an independent cursor and scroll position in each.
    Edits in one group move the other group's cursor along with the text.
  - `cmd-k cmd-w` closes a group, `cmd-k cmd-left/right` focuses the previous or next
    group, `cmd-k cmd-shift-right` moves the active tab to the next group.
- Menus (File, Edit, Selection, View, Help), one definition for every platform. macOS shows them
  in the system menu bar, with an application menu (About, Quit). Windows draws them in a
  custom title bar with minimize, maximize and close buttons and a drag area. Linux draws them
  in a row under the native title bar. Items are actions, so a click behaves like the
  shortcut, and shortcuts show as `⇧⌘P` on macOS and `Ctrl+Shift+P` elsewhere.
- Menu keyboard access in the in-window menus: `Alt+F/E/S/V/H` open a menu; arrows move,
  `Right` enters a submenu, `Left` leaves it, `Enter` runs, `Escape` backs out.
  Submenus are supported (View > Editor Tabs). Every title and item has a mnemonic, written
  `&File` in the menu definition and underlined while the menu bar has the keyboard; a letter
  runs the item (or opens the submenu) at once. Tapping and releasing `Alt` alone puts the
  keyboard on the bar: the first title is highlighted, arrows move between titles, `Down` or
  `Enter` opens one, a letter opens the menu with that mnemonic, `Escape` or `Alt` again leaves.
  Alt with another key or modifier is a shortcut, not a tap. macOS uses its system menu bar.
- Key bindings use the `secondary` modifier (Cmd on macOS, Ctrl elsewhere); line and word
  movement have per-platform bindings. `--chrome windows|linux|macos` previews another
  platform's menu and title bar layout.
- Open and Save As prompts complete directory entries (Tab completes, Enter on a folder
  browses into it). Tabs scroll horizontally and keep the active tab visible.

- Activity bar (explorer and panel buttons), side bar with a file explorer, and a bottom panel
  showing an output log (`cmd-b`, `cmd-j`). The explorer opens a workspace folder (the
  argument, the folder of the argument file, or the working directory), loads folders lazily,
  opens files into tabs, highlights and reveals the active file, and refreshes when files
  change on disk (`notify`, one watch per expanded folder).
- Split editors: `cmd-\` splits right, `cmd-k cmd-\` splits down, `cmd-k cmd-w` closes the
  split, `cmd-k cmd-right` focuses the other pane. Each pane has its own scroll position and
  can show any open tab, including the same tab as the other pane.

- Session restore: open files with their cursors and scroll positions, the group layout, side bar
  and panel state and the color theme come back when you start in the same folder with no
  file named. Saved files only; untitled documents and unsaved text are not kept. A group that
  held no saved file (only untitled or generated tabs) comes back as an empty untitled tab, so
  the grid keeps its shape and sizes; a group whose files have since been deleted drops out.
- Explorer: multi-select (command-click, shift-click, shift-arrows), cut, copy and paste of files
  and folders, and drag and drop to move them (or onto an editor to open them). Copy and cut
  put the paths on the system clipboard, one per line, so another window pastes them (a cut moves
  the files). Absolute paths copied as text from anywhere paste as a copy. Files copied in
  Finder or another file manager are not read: the toolkit only gives text and images.
- Search (`cmd-shift-f`): workspace text search with the engine behind ripgrep. It honors
  `.gitignore`, offers case, whole word and regex options, searches as you type, and opens a match
  with the match selected. "Files to include" and "files to exclude" take comma-separated globs
  (`*.rs, src/**`; a folder name covers what is inside it), and exclude wins. Tab walks through
  the four boxes.
- Replace in files: a replace box under the search box. Matches show the old text struck through
  and the new text beside it. Replace one match, every match in a file, or everything
  (`cmd-alt-enter`). Regex replacements expand `$1`, also when letters follow it (`$1rem`). Open
  files change in the editor and stay unsaved, so one undo reverses it; other files are rewritten
  on disk in the background. A match that moved since the search is skipped.
  A close button on each result (file or match) takes it out of the list, so a replace leaves it
  alone. `File: Save All` (`cmd-alt-s`) saves every open file with changes, and the search view
  offers it right after a replace.
- Fonts: the interface uses Inter (proportional, for reading); code, search patterns and the
  terminal use JetBrains Mono without ligatures (so columns stay exact). Both are bundled
  (SIL Open Font License, licenses in `apps/rustcode/assets/fonts`). `--font NAME` and
  `--code-font NAME` choose other installed families.
- Current line: the line with a caret gets a subtle band across the editor (theme key
  `editor.lineHighlightBackground`, translucent default) and a brighter line number. Only the
  active group shows it, and selections that span text do not.
- Window: the first run opens maximized. After that, size, position and maximized or full
  screen state are kept in `window.txt` in the user data directory (checked twice a second, so a
  quit or crash keeps them). Open files, layout and theme come back too when no file is named.
- Panel borders: thin lines separate the activity bar, side bar, editor, bottom panel, status bar
  and split dividers. Color comes from the theme (`panel.border`, `sideBar.border` or
  `editorGroup.border`), with a translucent grey default.
- Markdown preview, three ways. Three buttons at the right of the tab bar of a `.md` file (also
  the View menu and the command palette):
  - Side by side (`cmd-shift-v`): the page beside the editor.
  - New panel (`cmd-k v`): the page in a new group to the right.
  - Full page (`cmd-alt-v`): the page in place of the editor; press again to go back.
  Scrolling is linked both ways, as in VS Code: scroll the editor and the page follows to the
  same spot; scroll the page and the editor follows. The block that holds the caret is shaded (with no editor on screen, the page scrolls to keep
  that block in view). It updates as you type. Headings (no rule under them; use `---` for one), bold, italic, strikethrough, inline
  code, links, lists (nested, numbered, task), quotes, code blocks (colored by language),
  tables, rules and pictures from files beside the document. Links open in the browser (web) or
  the editor (files). Web pictures show as a link. Not yet: following the caret by column.
- Search panel: toggle labels and hints (`Search`, `Replace`, example globs) now have readable
  contrast, and unfocused boxes have a visible border.
- Long lines: word wrap (`alt-z`, View menu, or `View: Toggle Word Wrap`). Markdown and text files
  wrap by default; other files let lines run on. Wrapping breaks after spaces, or inside a word
  longer than a row, and counts CJK and emoji as two cells. Line numbers show on the first row of
  a line, the current-line band covers every row of it, and clicks, selections, the caret,
  scrolling to the caret and the Markdown preview sync all work in rows. Without wrap, the editor
  scrolls sideways (trackpad), line numbers staying put, and moving the caret off screen scrolls it
  back. Code blocks in the Markdown preview scroll sideways too; paragraphs wrap. Up and
  down move by wrapped row, keeping the column within the row; tabs count as four cells when
  choosing breaks.
- Empty state: with every tab closed (or a folder opened with no file), the editor area shows
  the name and four buttons: Open File (`cmd-o`), Open Folder (`cmd-k cmd-o`, also in the File menu
  and palette; it makes the folder the workspace), New File, and Command Palette. Opening or
  creating a file replaces it.
- Caret blink: the caret fades to faint and back about once a second, and stays solid while you
  type. It applies to every text input (editor, find box, palette inputs); the terminal's block
  cursor blinks the same way while the terminal has focus.
- Mouse pointers: an I-beam over text areas and inputs (editor, find box, terminal), a pointing
  hand over clickable rows, tabs, buttons and menu items, and a resize pointer on the splitters.
- Preferences: `File > Preferences` and the palette open `settings.json` (`~/.rustcode/`, which
  wins over VS Code's), the workspace's `.vscode/settings.json`, and `keybindings.json`, creating
  them with a commented template of every formatter, linter and language-server key.
  `Default Formatter...` and `Linters...` pick from lists and write the setting for you.
- Source control: branch name, staged and unstaged changes, stage and unstage one file or all,
  and commit, through libgit2.
- Source control, continued (all in the palette as `Git: ...`; the Source Control view has the
  common ones):
  - Gutter marks for lines changed since the last commit (blue modified, green added, red wedge
    for removed lines), live as you type.
  - Line blame: after the line with the caret, in grey, who last changed it, their username (the part of the
    email before `@`), when, the commit message, and "not pushed" when the commit is not on the
    upstream; lines you changed show "You · uncommitted changes". `Git: Toggle Line Blame`
    cycles the caret line only (the default), every line, and off.
  - Changes open as a colored unified diff, or side by side (committed text left, file right,
    changed lines tinted, scrolling together). Discard asks first and reloads the open file.
  - Stage Change at Cursor stages only the hunk under the caret.
  - File History lists the commits that changed the active file; picking one opens its diff.
  - Fetch, Pull (fast-forward only) and Push (sets the upstream the first time) run through the
    `git` program, so your credentials and SSH keys apply. The view and status bar show how far
    ahead or behind the branch is.
  - Switch Branch lists local branches, and a name that does not exist offers Create Branch;
    open files without unsaved changes reload after a switch or pull. A switch that would lose
    changes is refused.
  - Merge conflicts: marker lines and both sides are tinted, and Accept Current, Incoming or Both
    resolves the conflict under the caret.
  - The explorer colors changed files and the folders holding them, with a letter on files
    (M, A, D, U, !).
  - Stash Changes (with an optional message), Pop, Apply and Drop Stash; Create Tag (`name` or
    `name: message` for an annotated tag), Check Out Tag, Delete Tag; Merge Branch and Rebase onto
    Branch with Abort and Continue (the Source Control view shows "A merge is in progress" with
    Abort, and Continue for a rebase); Amend Last Commit (a toggle under the Commit button, or the
    command); Add Remote (`name url`) and Remove Remote.
  - Show Graph opens the Graph tab: all branches with colored lanes, merge and branch lines,
    branch and tag labels, and a mark for signed commits. Click a commit to read its changes.
  - Interactive Rebase: pick the oldest commit to rebase from; a "Rebase Steps" tab lists the
    commits as `pick` lines. Edit them (`pick`, `squash`, `fixup`, `drop`, `edit`; move lines to
    reorder), then `Git: Run Interactive Rebase from List`. `reword` is refused because there is no
    message editor; use `edit` and Amend. Skip and Continue show while a rebase is stopped.
  - Cherry-Pick Commit lists commits on other branches this branch lacks; Continue and Abort
    show in the Source Control view while one is stopped on a conflict.
  - Stash Current File, or the stash button on a changed file in the Source Control view,
    stashes that file only.
  - Submodules lists them with their state and updates one (`--init --recursive`); Update All
    and Sync URLs are commands.
  - Signing: the Source Control view cycles "per git config", "always" and "never"; `Git: Show
    Signing Setup` reports the key and `commit.gpgsign`. Commits go through the `git` program, so
    hooks run; what hooks print goes to the Output tab, which opens when a hook refuses a commit.
  - Pull requests use the GitHub CLI (`gh`): Create Pull Request, Check Out Pull Request, and
    Open Pull Request in Browser.
  - Open Merge Editor shows a conflicted file as three panes: current, incoming and the result.
  - A change can be staged by clicking its gutter mark, or put back as committed with
    alt-click (also `Git: Revert Change at Cursor`).
  - When a push, pull or fetch needs a name and password, the app asks for them (the password
    shows as dots) and tries again; they are kept in memory for the session only.
- Terminal (`ctrl-\``): a real shell in a pseudo-terminal, with colors, scrollback, resize,
  bracketed paste and the usual control keys, in the bottom panel. The panel resizes by dragging
  its top edge.
  - Selection: drag, double-click (word or path), triple-click (line), shift-click to widen.
    `cmd-c` (`ctrl-c` on Linux and Windows, which still interrupts the shell when nothing is
    selected) copies; typing clears the selection. The selection stays on its text while output scrolls.
  - Several terminals: the `+` button or `Terminal: New Terminal` adds a shell; tabs appear when
    there are two or more; `Terminal: Focus Next/Previous Terminal` and `Terminal: Kill Terminal`.
    Output of shells out of view is still collected.
  - Paths: `cmd-click` (`ctrl-click` on Linux and Windows) on `src/main.rs:10:5`, `a.cs(12,4)`,
    `./x`, `../x`, `~/x` or an absolute path opens the file at that line and column. Relative
    paths are tried against each workspace folder. A path that does not exist is reported.
- Problems: `cargo check` runs in the background after each save in a Cargo workspace. Errors
  and warnings are listed in the panel, marked in the gutter, and counted in the status bar.
  - Language servers (stdio, started when the first matching file opens): `rust-analyzer` for
    `.rs`; `pyright-langserver`, `basedpyright-langserver` or `pylsp` for Python;
    `typescript-language-server` for TS and JS; `gopls`; `clangd`. Programs are looked up on
    `PATH` plus `~/.cargo/bin`, `~/.local/bin`, `~/go/bin`, Homebrew and `/usr/local/bin`. Their
    problems appear in the Problems tab as you type (the whole text is sent every 250 ms after a
    change), and a file a server has reported on shows the server's list instead of the
    `cargo check` list. A missing or crashed server is reported in the status bar with its
    own error line, which names the program to install (for example `rustup component add
    rust-analyzer`). The same message shows when a hover or a suggestion list finds no server;
    pointer hover never says it (it asks quietly). Only diagnostics are used so far.
  - The Problems tab only shows what has been found: opening it runs nothing, and says "No
    problems have been detected" until a save (or `Run diagnostics`) runs the checks.
  - Squiggles: a wavy underline under each problem in the editor, colored by severity, over
    the syntax colors; the gutter dot stays. A problem with no width marks one character.
- Icons: file and folder icons are the [Material Icon Theme](https://github.com/material-extensions/vscode-material-icon-theme)
  (MIT, version 5.39: 1251 icons, 1381 extensions, 2148 file names, 4659 folder names such as `src`,
  `node_modules` or `.github`; its license is kept in `apps/rustcode/assets/icons`). It is the bundled
  VS Code icon theme and the default; `--icon-theme FILE` loads another. It adds about 5 MB to the
  program. Interface icons (activity bar, tabs, chevrons, status bar, window controls, the
  debugger) are [Phosphor Icons](https://phosphoricons.com) (MIT, license in `assets/ui`), tinted
  with the theme's colors, in the bold weight when small. `scripts/material_icons.py` imports the
  Material icons from the npm package, and `scripts/generate_icons.py` makes the Phosphor interface
  icons from the copies in `scripts/phosphor` and lists everything in `apps/rustcode/src/assets.rs`.
  To use another interface icon (or Hugeicons) change the table at the top of the second script.
- Application icon (`apps/rustcode/assets/app-icon.svg`): braces around a text caret. `scripts/
  make_app_icons.sh` renders it to PNG, `.icns` and `.ico`. The dock shows it while running, and
  `scripts/bundle-macos.sh [debug]` builds `target/bundle/RustCode.app` with it.
- A file argument opens the project around it (nearest `.git`, `Cargo.toml`, ...), not just its
  folder.

## Phase 3: Language intelligence (done)

Done:

- Linters. Programs that check a file and report into the Problems tab, the gutter marks, the
  squiggles and the status bar counts, next to what language servers and `cargo check` report (a
  problem two sources both found is listed once).
  - Programs: `eslint` (JavaScript, TypeScript, their React forms, Vue; the project's
    `node_modules/.bin/eslint` wins), `ruff check`, `flake8`, `pylint`, `mypy` and `pyright` (Python),
    `stylelint` (CSS, SCSS, Less), `yamllint`, `actionlint` (GitHub Actions workflows only, under
    `.github/workflows/`), `tflint` (Terraform, where a `.tflint.hcl` is), `hadolint` (Dockerfiles), `shellcheck`,
    `markdownlint` (markdownlint-cli), and `golangci-lint` (Go; both the version 1 and 2 flags are
    tried, and it lints the whole module, so its findings replace the module's earlier ones).
    ESLint is given the file's path from its project folder, as it ignores a path it cannot find
    under its own working folder (a symlinked `/var` on macOS).
  - When: on open (a file is linted the first time it is in front) and on save, in the background
    (90 s limit). With `"rustcode.lint.run": "onType"` the text in the editor is linted too, once
    it has rested for 0.7 s, by sending it to the program's standard input (so unsaved problems
    show); `mypy` and `golangci-lint` can only read files and wait for the save. `Lint: Run
    Linters on This File` runs them now. A save during a run queues one
    more run. Changing the lint settings drops what was found and lints the file in front again.
  - Which: with nothing set, a linter runs where the project is set up for it: an ESLint config or
    `eslint` in `package.json`; `ruff.toml` or `[tool.ruff]`; `.flake8` or `[flake8]` in
    `setup.cfg`/`tox.ini`; `.pylintrc` or `[tool.pylint]`; `mypy.ini` or `[tool.mypy]`; a
    stylelint config; a `.yamllint` file; a markdownlint config; a `.golangci.*` file (and a
    `go.mod`). `shellcheck` and `hadolint` run wherever they are installed. A program that is not installed is skipped quietly,
    unless the settings named it: then the status bar says so, once.
  - Settings (each also per language, `"[python]": {...}`): `rustcode.linters` (a list of names; it
    replaces the automatic choice, and `[]` runs none), `rustcode.linters.disabled` (names to leave
    out), `rustcode.lint.run` (`"onSave"`, the default, `"onType"` or `"off"`).
  - Clippy is the project's check command: `"rust-analyzer.check.command": "clippy"`, or `clippy` in
    `rustcode.linters` for Rust (and not disabled), runs `cargo clippy` where `cargo check` ran.
    Its lints are labelled `clippy` in the list.
  - Quick fixes (`cmd-.`), for the problems at the caret or selection: the linter's own (ESLint
    fixes and suggestions, Ruff fixes with unsafe ones marked, ShellCheck, markdownlint), then
    "Disable <rule> for this line", which every linter gets: `# noqa: CODE` (flake8, ruff),
    `# pylint: disable=`, `# type: ignore[code]`, `//nolint:linter`, `# yamllint disable-line`, or a
    comment line above (`// eslint-disable-next-line`, `# shellcheck disable=`,
    `<!-- markdownlint-disable-next-line -->`, `/* stylelint-disable-next-line */`,
    `# hadolint ignore=`); an existing list of codes on the line is extended. "Fix all <tool>
    problems in this file" makes every fix the linter offered, never the comments. Fixes are
    offered only while the text is the one the linter read; after an edit they wait for the next
    run. This is how flake8 and golangci-lint, which report no fixes, get one.
  - "Disable <rule> for this file": a comment at the top of the file (after a shebang) for every
    linter that has one (`# ruff: noqa: CODE`, `# flake8: noqa: CODE`, `# pylint: disable=`,
    `# mypy: disable-error-code=`, `# pyright: rule=false`, `/* eslint-disable rule */`,
    `# shellcheck disable=`, `<!-- markdownlint-disable -->`, `/* stylelint-disable */`,
    `# yamllint disable rule:`, `# hadolint global ignore=`); not for golangci-lint, whose run does
    not know the file's text.
  - Stylelint reports problems without fixes but can fix a text itself: once it has found
    something, `stylelint --fix` runs on the text (standard input), and its changes become
    "Fix all stylelint problems in this file". `pylint` has no fixes to offer.
  - Checked live against the real tools (ESLint 9, Ruff 0.17, flake8 7.3, pylint 3.3, mypy 1.19,
    stylelint 17, yamllint 1.37, hadolint 2.15, ShellCheck 0.11, markdownlint-cli, golangci-lint 2.15,
    clippy): `crates/infrastructure/tests/linters.rs` and `clippy.rs` run each one on files and on
    typed text, make its fixes and "Disable..." comments, and lint again to see the problem go;
    they skip a tool that is not on `PATH`. Running them against real output found and fixed
    differences from what the parsers first assumed: ShellCheck counts a tab as one column, Ruff
    0.17 reports its own severity, ESLint ignores symlinked paths, markdownlint prints nothing for
    a clean file, and markdownlint and stylelint print their JSON on the error stream.
  - `pyright` reads files only, so the linter runs on open and save; while typing, its diagnostics
    come from the language server (`pyright-langserver` is one of the servers started for Python).
    `tflint` also reads the module's files (it takes `# tflint-ignore: rule` for a line and
    `# tflint-ignore-file: rule` for the file). `actionlint` takes standard input.
  - Not yet: linters for other kinds of file; the list above is the set built in.

- Formatter. Format Document (`shift-alt-f`), Format Selection (`cmd-k cmd-f`) and format on save
  (`editor.formatOnSave`, also per language) run a formatter *program* when one applies, else the
  language server as before. The file's text goes to the program on standard input and the result
  is applied as a diff: one undo step, only the changed stretches, carets in untouched text stay.
  A program runs in the background (15 s limit); a save waits up to 10 s for it, then goes ahead.
  A program that fails (the file does not parse) or prints nothing leaves the file alone and the
  status bar says why.
  - Programs, found on `PATH`, `~/.cargo/bin`, `~/go/bin`, Homebrew, nvm and the like:
    `rustfmt` (edition from the nearest `Cargo.toml`, or the workspace's), `gofmt`, `ruff format`
    then `black` (Python), `clang-format` (C, C++, Objective-C), `shfmt`, `taplo` (TOML), `stylua`
    (Lua), `zig fmt`, `google-java-format`, `terraform fmt`, `swiftformat`, `ktfmt`, `elm-format`,
    `nixfmt`, `mix format`, `scalafmt`, `ocamlformat`, `gleam format`, `rubocop` (formatting cops only), and `prettier`
    (JavaScript, TypeScript and their React forms, JSON, CSS, SCSS, Less, HTML, Vue, Markdown,
    YAML; the project's `node_modules/.bin/prettier` wins over the one on `PATH`).
  - Choosing: `editor.defaultFormatter` takes a VS Code extension id (`esbenp.prettier-vscode`,
    `rust-lang.rust-analyzer`, `ms-python.black-formatter`, `charliermarsh.ruff`, `golang.go`) or a
    tool name, per language with `"[python]": {...}`. A chosen tool that is not installed is an
    error. Any other value means the language server. When nothing is set the usual tool for the
    language is used if it is installed, except Prettier, which is used only when the project asks
    for it (a Prettier config file, or `prettier` in `package.json`); otherwise the language server
    formats.
  - Config files are honored by the tools themselves: they run in the file's folder with the file's
    name, so `rustfmt.toml`, `.prettierrc`, `pyproject.toml`, `.clang-format` and `.editorconfig`
    apply as they do on the command line.
  - Format Selection formats the whole file with the program and keeps only the changes that touch
    the selected lines (the caret's line when nothing is selected). A change that keeps its line
    count is split per line, so neighbouring lines can be told apart; where one change spans lines
    that were not selected too, the whole change is made. With no program, the language server's
    range formatting is asked (`textDocument/rangeFormatting`).
  - `editor.formatOnPaste`: the pasted lines are formatted the same way (a single caret; errors are
    silent), by the program or the server's range formatting.
  - `editor.formatOnType` (off by default, as in VS Code): after `;`, `}` or a newline, the line just
    typed (after a newline, the line it finished) is formatted by the program, one run at a time and
    silently, so code that does not parse yet is left alone. With no program, the server is asked
    (`textDocument/onTypeFormatting`) after the characters it names in its capabilities. The answer
    is dropped if you kept typing, so it takes effect when you pause. Files over 400,000 characters
    are skipped.
  - "Format Document With..." (command palette) lists the programs that suit the file, each marked
    installed or not, and the language server; the choice applies to that run.
  - `.editorconfig` (nearest files up to `root = true`, with the format's globs): `indent_style`,
    `indent_size`, `tab_width`, `trim_trailing_whitespace` and `insert_final_newline` override
    `settings.json`. Indentation follows the file in front (it is global, so two split files with
    different indentation show the one in front's), and the save rules apply on every save.
    `rustfmt` gets the indentation as `--config` when the project has no `rustfmt.toml`, and
    `shfmt` gets `-i` from the editor's indentation when no `.editorconfig` speaks.
  - Checked live against the installed `rustfmt` and `gofmt` (tests in
    `crates/infrastructure/src/program_formatter.rs` skip a tool that is not installed); the other
    tools are covered by their command lines (`crates/application/src/formatter.rs`) and a stand-in
    program, not by running them.
  - Windows: programs are found with `.exe`, `.cmd`, `.bat` and `.com` endings (so `prettier.cmd`
    in `node_modules/.bin` and npm's `%APPDATA%\npm` shims work), and the usual Windows tool
    folders are searched. The lookup is tested on macOS with the endings passed in, and the
    Windows-only code (`programs.rs`, `process.rs`) compiles for `x86_64-pc-windows-msvc`. The
    infrastructure crate was also cross-compiled for `x86_64-pc-windows-gnu` (with `zig` as the C
    compiler and linker) and its program-lookup and shim tests run under Wine, including a project's
    `.cmd` shim and a bare program found as `.exe`. That found and fixed two bugs: a test that
    used a Unix-only API without a gate, and a test double that no longer compiled. It is Wine,
    not Windows itself. The whole editor (GPUI included) also compiles and links as
    `rustcode.exe` for `x86_64-pc-windows-gnu`. Run under Wine with DXVK it creates its Direct3D
    device and compiles its shaders, then stops at Wine's stub of
    `CreateSwapChainForComposition`, which the Windows renderer needs ("failed to open window:
    Creating DirectX renderer"), so the window itself has not been seen.
  - "Configure Default Formatter..." (command palette) lists the same choices and saves the one you
    pick as `editor.defaultFormatter` for the file's language (`"[python]": {...}`) in
    `~/.rustcode/settings.json`, editing only that entry: comments, trailing commas and the other
    entries stay as they were. It says when a workspace settings file overrides the choice.
  - Run for real, through the real runner and command lines (`crates/infrastructure/tests/
    formatters.rs`; each test skips a program that is not installed): `rustfmt`, `gofmt`, `black`,
    `ruff format`, `prettier` (with a project config), `clang-format`, `shfmt` (with the editor's
    indentation), `taplo`, `stylua`, `rubocop`, `elm-format`, `terraform fmt`, `swiftformat`,
    `zig fmt`, `google-java-format`, `ktfmt`, `gleam format`, `dart format`, `ormolu`, `csharpier`
    (1.x: `format --write-stdout --stdin-path`; found as `csharpier` or `dotnet-csharpier`),
    `mix format`, `scalafmt` (reads the project's `.scalafmt.conf`), `ocamlformat` (formats files
    outside a project too, with `--enable-outside-detected-project`) and `nixfmt` (0.6). Every
    program in the list above was run.
  - `rubocop` runs with `--only Layout`: its other corrections change the code (it deletes an
    assignment nothing reads), and its exit code 1 ("offenses are left") counts as formatted.

- Bracket pairs and guides. The highlighter records the `() [] {}` that the grammar's scopes say
  are code (not `string*`, `comment*` or inline code, but `${...}` and f-string fields are code
  again), and the pairs and nesting depth are worked out from that (`domain::BracketMap`).
  - Colorization: brackets take a color by nesting depth, across all three kinds
    (`editorBracketHighlight.foreground1..6`, levels the theme leaves transparent are skipped;
    Dark+ and Light+ use VS Code's three defaults). A bracket with no partner uses
    `editorBracketHighlight.unexpectedBracket.foreground`. `editor.bracketPairColorization.enabled`,
    and "Toggle Bracket Pair Colorization" in the palette.
  - Active pair: with the caret next to a bracket, both brackets of the pair get a box
    (`editorBracketMatch.background` and `.border`; `editor.matchBrackets`, `never` turns it off).
  - Bracket pair guides: a vertical line down the block of each pair that spans lines, at the
    indentation of its opening line, in the level's color, faint, and full for the pair around the
    caret (`editorBracketPairGuide.background1..6` and `activeBackground1..6` when the theme
    names them). `editor.guides.bracketPairs` is `false` (default, as in VS Code), `"active"` or
    `true`; `editor.guides.highlightActiveBracketPair`. "Cycle Bracket Pair Guides" in the palette
    changes it for the session.
  - Indentation guides (`editor.guides.indentation`, default on): one line at each indent step,
    the caret's block brighter (`editor.guides.highlightActiveIndentation`;
    `editorIndentGuide.background1`/`activeBackground1`). Blank lines take the smaller indent of
    their neighbors. The active guide yields to a lit bracket pair guide.
  - `Go to Bracket` (`cmd-shift-\`, Selection menu): to the other side of the partner of the
    bracket by the caret (before `(` goes after `)`, so repeating comes back); inside a pair it goes
    to the closing bracket. `Select to Bracket`: the text between the brackets, then with them.
  - `<>` pairs: generics and type parameters (Rust, TypeScript, Java, C#, C++ templates) pair and
    color like the other brackets; the grammar's scopes tell them from comparison operators, so
    `a < b` is left alone. The `<` and `>` of tags in HTML, XML and JSX pair too, each tag a pair of
    its own. The C++ grammar takes the template of a declaration (`std::vector<int> v;`) for a
    comparison, so for C++ the text decides: a `<` right after a name, closed by a `>` with only
    names, `::`, commas, `*`, `&` and nested templates between, and no name or `=` after it.
    `if (a<b && c>d)`, `i<n; i++`, `x<<2` and `a<=b` are left alone.
  - `editor.bracketPairColorization.independentColorPoolPerBracketType`: each kind counts its own
    nesting for its colors.
  - `editor.guides.bracketPairsHorizontal` (`"active"` by default, `true`, `false`): a line on the
    row of a closing bracket that starts its line and is indented more than its pair's guide,
    from the guide to the bracket.
  - Files with no grammar (plain text) get brackets from their text; a bracket with no partner
    next to the caret says "No matching bracket" in the status bar; the guides of a wrapped
    line stay where its block is on every row.
  - Wrapped lines: `editor.wrappingIndent` (`same`, the default, or `none`; `indent` and
    `deepIndent` count as `same`) starts each row after the first of a line as far in as the line
    itself, never more than half a row. The text is clipped from there, so it does not show the
    characters before it; clicks, the caret and popups use the indented position; the guides run
    through all the rows.
  - Not yet: other languages' angle-bracket pairs that their grammar does not scope (and that the C++
    text rules above do not cover), and `indent` / `deepIndent` as separate wrapping modes.

- LSP client: diagnostics (Phase 2), hover, go to definition, completion, find references, rename,
  formatting, code actions, parameter hints, document and project symbols, implementations and
  type definitions, call hierarchy, folding ranges, document links, inlay hints, linked editing.
- Semantic tokens from language servers.
- Integrated terminal (Phase 2).
- DAP client for debugging (below).

Details of what was built, on the language server connection of Phase 2 (same servers, same
files; questions go out through the `LanguageServers` port and the answers come back a moment
later, matched by request number):

- Hover: `Editor: Show Hover` (`cmd-k cmd-i`) at the caret, or rest the pointer on a word for
  about half a second. The text is the server's, with code fences dropped (Markdown is not
  rendered yet). It goes away when you type, click or move to another word. Colors:
  `editorHoverWidget.*`.
- Go to Definition: `F12` or `cmd-click` (`ctrl-click` elsewhere). One place opens at once with
  the name selected; several are listed in the palette. Places in files that are not open are
  read from disk to get their columns right.
- Completion: `ctrl-space`, and automatically after `.` and `::`. The list opens under the word
  with a short kind (`fn`, `struct`, `var`, ...) and the signature; typing narrows it by fuzzy
  match on what has been typed, matched letters in the highlight color; `up` and `down` move
  (and wrap), `enter` or `tab` put the choice in, `escape` or leaving the word closes it.
  Snippet syntax in suggestions is flattened to plain text (no tab stops yet), and the server's
  own edit range is honored. Up to 300 suggestions; colors `editorSuggestWidget.*`.
- Find All References (`shift-F12`) lists every place in the palette with the text of its line,
  even when there is only one.
- Rename Symbol (`F2`): the box opens with the word under the caret selected; the server's
  changes are made in every file. Files that are open change in the editor as one undo step and
  stay unsaved; files that are not open are written to disk. One caret is left where it was.
- Format Document (`shift-alt-f`): the server's edits are applied as one undo step; if the text
  changed while the server was working, formatting is skipped. `"editor.formatOnSave": true`
  formats first and then saves (a server that does not answer within 3 seconds, or fails, does
  not stop the save).
- Code Actions (`cmd-.`, `ctrl-.` elsewhere): the actions for the selection or caret, with the
  problems there sent along, in the palette (the server's preferred one first). Taking one makes
  its changes; an action with no changes yet is resolved first; one with a command runs it, and
  changes the server then asks for by itself (`workspace/applyEdit`) are made too.
- Parameter Hints: after `(` and `,` (and `ctrl-shift-space`), a box above the caret shows the
  signature with the active parameter in bold and the highlight color, `1/2` for overloads, and
  the first line of the documentation. It closes after `)`, on `Escape`, and when the caret
  leaves the line or goes back before the call.
- Symbols: the file's symbols are asked for after it has rested for 0.6 s and are kept per file.
  The breadcrumbs continue the path with the names that hold the caret (click one to go to it).
  `Go to Symbol in Editor` (`cmd-shift-o`) lists them with their nesting and filters as you type.
  `Go to Symbol in Workspace` (`cmd-t`) asks the server as you type and shows the answer to what
  is typed now (older answers are dropped).
- All of these give UTF-16 columns to servers and read them back as characters, so emoji and
  CJK text land in the right place. The file is sent to the server (full text) before each
  question, so the answer is about what is on screen.
- Checked live against `typescript-language-server` with TypeScript 5: members of an object
  after `.`, hover, definition, references, rename of a function across its uses, formatting,
  document symbols and the symbol breadcrumbs, the quick fix "Add missing properties", and
  signature help. `rust-analyzer` needs the
  rustup component; Python, Go and C servers were not tried. Newer `typescript-language-server`
  with TypeScript 7 does not start its project, which is the servers' doing.
- Go to Implementations (Cmd-F12) and Go to Type Definition use the definition list; several
  answers open the palette.
- Call hierarchy: "Show Call Hierarchy: Incoming Calls" (Shift-Alt-H) and "Outgoing Calls" list
  the callers or callees in the palette, with how many call sites each has.
- Folding: ranges come from the server, or from indentation when there is none or it fails.
  Chevrons in the gutter (shown on hover, always when folded), a `⋯` badge after a folded line,
  Fold / Unfold (Cmd-Alt-[ and ]), Toggle Fold (Cmd-K Cmd-L), Fold All (Cmd-K Cmd-0), Unfold All
  (Cmd-K Cmd-J). Folded lines have no rows, so they work with word wrap too. Ranges move with edits
  and are dropped when an edit crosses their ends. A jump or search into a folded line opens it.
  Folds are kept per open file, not saved with the session.
- Document links: URLs and paths the server finds are underlined in the link color
  (`textLink.foreground`); Cmd-click opens them. Links the server leaves unresolved are not
  followed. (typescript-language-server has no document links.)
- Inlay hints: types and parameter names show in the text (`editorInlayHint.*` colors), asked for
  the lines in view plus a margin. `editor.inlayHints.enabled` and the "Toggle Inlay Hints" command
  switch them. Not shown in wrapped text. Hints disappear while the file is being edited and come
  back when it rests.
- Linked editing: with the caret in a tag name (HTML, JSX), typing, Backspace and Delete change
  the twin name too, in the same undo step. Mirroring stops at a character that cannot be part of
  a name, such as a space. Only a single caret is mirrored.
- Checked against typescript-language-server with the ignored test
  `typescript_server_answers_the_remaining_requests`.
- Debugging (Debug Adapter Protocol), through adapters run as child processes (the editor only talks
  the protocol): `lldb-dap` (Rust, C, C++, Swift; from Xcode or LLVM), `debugpy` (Python;
  `pip install debugpy`), and `js-debug` (Node and Chrome pages; "Debug: Install JavaScript
  Debugger" fetches it into `~/.rustcode/js-debug`, or set `RUSTCODE_JS_DEBUG`).
  - Floating bar: while a session runs, a bar floats at the top of the editor with icons for
    Continue/Pause, Step Over, Step Into, Step Out, Restart and Stop (hover for the key); drag its
    grip sideways to move it. The same buttons are in the Debug tab, and everything is in the new
    **Run** menu and the palette.
  - Breakpoints: click beside a line number (a faint dot shows where) or `F9`. They follow edits,
    are listed in the Debug tab (click to go there, `×` to remove), go to the adapter at once
    during a session, are kept with the folder's session, and turn grey when the adapter cannot
    place them. "Add Conditional Breakpoint...", "Add Logpoint..." (a message with `{expressions}`,
    shown as a diamond, no stop) and "Add Hit Count Breakpoint..." (`5`, `>3`, `%2`) work on the
    caret line; a condition or hit count shows a pale dot inside the red one.
  - Watch expressions ("Debug: Add Watch Expression...", kept with the session) are asked for at
    each stop and each frame change and show above the variables; with the program stopped,
    resting the pointer on a name (or a selection) shows its value.
  - Start: `F5`. With `.vscode/launch.json` (comments allowed; `lldb`/`codelldb`/`cppdbg`,
    `python`/`debugpy`, `node`/`pwa-node` and `chrome`/`pwa-chrome` launch entries, `attach` entries
    for LLDB with a `pid` or `${command:pickProcess}`; `${workspaceFolder}`, `${file}` and similar
    variables; `preLaunchTask` from `.vscode/tasks.json` shell, process and cargo tasks; `"console":
    "integratedTerminal"`) it runs those; with none, a Rust project is built with `cargo build` and
    each program is offered, a Python or JavaScript file runs as it is. Several choices open a
    picker. Unsaved files are saved first. "Debug: Attach to Process..." lists running processes.
  - Program input: with `"console": "integratedTerminal"`, or "Debug: Start Debugging in
    Terminal", a Python or Node program runs in a new shell of the Terminal tab, so it can read the
    keyboard. Otherwise, and always for `lldb-dap` (its launcher cannot attach from a terminal), the
    output shows in the debug console and the program cannot read the keyboard.
  - While stopped, the editor shows the line (yellow band and arrow) and the **Debug** tab of the
    panel shows breakpoints, call stack (click a frame), variables (open structures; Locals,
    Globals, Registers) and the console. `F5` continues, `F10` steps over, `F11` into,
    `Shift-F11` out, `F6` pauses, `Shift-F5` stops, `Cmd-Shift-F5` restarts; "Debug: Evaluate
    Expression..." evaluates in the chosen frame. The status bar says running or paused, and VS
    Code's `workbench.action.debug.*` commands work in `keybindings.json`.
  - Checked live: `lldb-dap` with a Rust program (build, a conditional breakpoint that skipped the
    first call, stop, variables, step, continue, output, exit code); the ignored tests
    `lldb_dap_stops_at_a_breakpoint_in_a_rust_program`,
    `debugpy_stops_at_a_breakpoint_in_a_python_file` and
    `js_debug_stops_at_a_breakpoint_in_a_node_program` drive the real adapters. A scripted adapter
    covers the protocol (conditions, terminal requests, reverse requests) in the normal tests.
    Terminal mode is checked against `debugpy` (`debugpy_runs_the_program_in_a_terminal_when_asked`). The Install command is checked by `the_javascript_debugger_can_be_fetched`. Not tried live: Chrome pages.
  - Not yet: data breakpoints and exception filters, editing a variable's value, a disassembly
    view, several debug sessions at once, and breakpoints in files with no path.
- Semantic highlighting: after a file rests (0.6 s) the server's token kinds and modifiers
  (`function`, `parameter`, `variable` + `readonly`, ...) color names over the grammar's colors.
  The theme's `semanticTokenColors` rules win; where it has none, the token is colored like the
  TextMate scope VS Code maps it to (a readonly variable like a constant, an enum member, a
  namespace). While you type, the lines you touch lose their server colors until the next answer
  and later lines move along. `"editor.semanticHighlighting.enabled": false` turns it off. Not
  yet: modifiers like `deprecated` (strikethrough), range requests and deltas for huge files.
- Pictures, sound and video: PNG, JPEG, GIF, WebP, BMP, ICO and TIFF open in a viewer tab (size
  and dimensions in a bar; "Actual size" / "Fit to window"). MP4, MOV, WebM, MP3, WAV and the like
  open in a card with "Open in Default Player", since the editor has no media decoder of its own.
  These tabs cannot be edited or saved over. SVG files stay text (colored as XML) and get the same
  three preview buttons as Markdown, drawn live from the text (a bad SVG says so).
- Colors for more files: Markdown (headings, bold, italic, code, links, quotes, lists), TOML,
  `Cargo.lock` and other lock files, `.gitignore` and the other ignore lists (comments, `!`, glob
  characters), `.env*`, `.ini`/`.cfg`/`.editorconfig`, diffs. Dark+ and Light+ got the missing
  rules; a loaded VS Code theme brings its own.
- Success (goal): Rust, TypeScript and Python are usable without extensions.

## Phase 4: Extension host (what extensions do in the editor and the workbench is done; the VS Code RPC host is not planned)

The plan is layered, so that most extensions need no JavaScript at all:

1. **Declarative extensions (done).** Themes, grammars, file types, language rules, snippets,
   icon themes and key bindings are read from `package.json` (`contributes.themes`, `grammars`,
   `languages` with their `language-configuration.json`, `snippets`, `iconThemes`, `keybindings`),
   with no runtime. Folders are scanned in `~/.rustcode/extensions`, in `--extensions DIR`, and in
   VS Code's own `~/.vscode/extensions`. `Extensions: Install from VSIX...` (command palette)
   unpacks a `.vsix` (a zip; only its `extension/` part, and entries that climb out of the folder
   are dropped) into `~/.rustcode/extensions/<publisher>.<name>-<version>` and uses it at once,
   with no restart. Paths in a manifest that are absolute or use `..` are refused, and a part that
   cannot be read is skipped and named in the Output tab.
   - Themes join the Color Theme picker under their label (once, even across versions).
   - Icon themes are listed by `Preferences: File Icon Theme`; the choice is saved as
     `workbench.iconTheme` in `~/.rustcode/settings.json`, and the setting can be edited by hand.
   - Grammars (`.json`, JSONC, or `.tmLanguage` plist) are registered with the TextMate engine;
     `languages` map extensions, exact file names and aliases to them, and open files are
     re-colored. An extension's grammar for a language the program already has takes over, as in
     VS Code.
   - Language rules: `comments` (line and block) and `autoClosingPairs`/`surroundingPairs`
     (defaulting to `brackets`) drive three editor features that did not exist before:
     `Edit: Toggle Line Comment` (`cmd-/`), `Edit: Toggle Block Comment` (`shift-alt-a`), and
     brackets and quotes that close themselves. Typing a bracket or quote closes it when the next
     character is a space, the end of the line, or a closing mark; a quote after a letter (an
     apostrophe) is left alone; typing the closing mark steps over it; backspace between a pair
     removes both; typing a mark over a selection wraps it. `editor.autoClosingBrackets: "never"`
     turns it off. About 40 file types have built-in rules for files no extension describes.
   - Snippets (VS Code's snippet file format) join the Ctrl+Space suggestions for their
     language, with or without a language server; the user's own go in
     `~/.rustcode/snippets/<language>.json`. Inserting a snippet (also a language server's) steps
     through its tab stops: `Tab` and `Shift+Tab` move between `$1`, `$2`, ... with each default
     selected, places that share a number (`$1` twice) are typed into together, `$0` ends the
     snippet, later lines follow the indentation of the line, and moving the caret away or
     reaching the end makes Tab an ordinary tab again. A stop with choices (`${1|u8,i32|}`) lists
     them like suggestions; typing instead closes the list. Variables take their values: the file
     (`TM_FILENAME`, `TM_FILENAME_BASE`, `TM_DIRECTORY`, `TM_FILEPATH`, `RELATIVE_FILEPATH`,
     `WORKSPACE_NAME`, `WORKSPACE_FOLDER`), the caret (`TM_LINE_INDEX`, `TM_LINE_NUMBER`,
     `TM_CURRENT_LINE`, `TM_CURRENT_WORD`, `TM_SELECTED_TEXT`), `CLIPBOARD`, the local date and time
     (`CURRENT_*`), `UUID`, `RANDOM`, `RANDOM_HEX`, and the language's `LINE_COMMENT` and
     `BLOCK_COMMENT_START`/`END`; `${NAME:default}` uses the default when the value is empty or
     unknown. Transforms work on variables and on mirrors of a stop (`${NAME/regex/format/g}`;
     `$1`, `${1:/upcase|downcase|capitalize|pascalcase|camelcase}`, `${1:+if}`, `${1:-else}`,
     `${1:?if:else}`, flags `g i m s`). A transformed repeat of a stop (`${1/regex/format/}`)
     follows the stop as you type in it, as part of the same undo step; it is not a place the
     caret visits.
   - Contributed key bindings come before the user's own, which win. `when` clauses work as in
     `keybindings.json`.
   - Indentation: `Enter` indents after an opening bracket (`{`, `(`, `[`) and splits a pair
     (`{|}`) into an indented line and the closing mark below it; `indentationRules` deepen the
     next line (`increaseIndentPattern`, `indentNextLinePattern`) and pull a line back as the
     last character that makes it match `decreaseIndentPattern` is typed (`else:`, `end`); a
     closing bracket typed alone on a line goes back a level; `onEnterRules` (before, after and
     previous-line text; `indent`, `indentOutdent`, `outdent`, `appendText`, `removeText`) win over
     all of these; lines matching `unIndentedLinePattern` (preprocessor lines, say) are skipped
     when Enter looks for the indentation to follow. Patterns are JavaScript regular expressions
     (lookahead works); one that does not compile is dropped. Built-in rules cover Python, YAML, Ruby, Lua and shell, and the
     brackets of every code file type.
   - `Preferences: Extension Settings...` lists the settings extensions declare in
     `contributes.configuration` (key, default or current value, description); choosing one
     writes it with its default into your settings (comments and the rest of the file are kept)
     and opens the file. `Extensions: Uninstall Extension...` lists what was installed here (not
     VS Code's own folder) and deletes the one you pick; its themes, grammars, file types,
     snippets, icon themes, key bindings and settings go away at once (the highlighter is
     rebuilt from the bundled languages plus what is still installed, and open files are
     re-colored). The Node extension host reads these settings with `workspace.getConfiguration`
     (`get`, `has`, `inspect`, sections such as `getConfiguration('nice.deep')`): the defaults
     extensions declare, with the user's own over them, given at start and again on every
     change, which fires `onDidChangeConfiguration` (with `affectsConfiguration`).
     `getConfiguration().update(key, value, target)` saves the setting keeping the file's comments
     and layout (`undefined` removes the entry), and the extension reads its own change at
     once. The target is the user's `~/.rustcode/settings.json` for `ConfigurationTarget.Global`
     or `true`, the workspace's `.vscode/settings.json` for `Workspace`, `WorkspaceFolder` or
     `false`; with none given, the workspace's file when it already has the setting, else the
     user's. (There is one workspace file, the first folder's.)
   - Brackets: an extension whose `language-configuration.json` lists `brackets` decides which
     characters count as brackets for matching, colors and guides in its language (a language
     that lists only `{}` and `[]` gets no pairing of parentheses). Pairs the program did not
     know are added: single characters (`«»`), and words or longer marks (`begin`/`end`,
     `do`/`end`, `<%`/`%>`). Several openers can share a closer, so Ruby's `class`, `def` and
     `do` each take the nearest `end`. A word counts only as a whole word and only where the
     grammar scopes it as a keyword (`keyword*`, `storage*`, `punctuation.section*`), so a
     variable called `end`, a string, a comment or `.end` as a method name is not a bracket;
     marks made of symbols are recorded outside strings and comments. The whole word is colored,
     boxed beside the caret, jumped over and selected by "Go to Bracket" and "Select to Bracket".
     Enter after a word that opens a block (`do`) indents, but not after `undo`.
2. **Language servers and debug adapters launched directly (done).** Built-in table of about 30
   servers (Rust, Python, TypeScript/JavaScript, Go, C/C++, JSON, HTML, CSS/SCSS/Less, YAML,
   TOML, Markdown, Bash, Lua, Zig, Ruby, PHP, Java, Kotlin, C#, Swift, Dart, Haskell, OCaml,
   Elixir, Clojure, Scala, Nix, Terraform, Svelte, Vue, LaTeX, Gleam, Dockerfile, CMake); each
   starts on the first file of its type, only when its program is found, and says how to install
   it when not. The `rustcode.languageServers` setting adds a server, replaces a built-in one by
   name or removes it (`"disabled": true`); an entry names its files by `extensions` and/or exact
   `filenames` (a name beats an extension), and may carry `initializationOptions` (sent with
   `initialize`), `settings` (answered to `workspace/configuration` by section, and sent in
   `workspace/didChangeConfiguration`) and `languageId`. Changing the setting restarts servers and
   re-opens files with the new ones, and an entry that cannot be used is reported. Debug adapters:
   `lldb-dap` (Rust, C, C++, Swift), `debugpy`, `js-debug`, and Delve (`dlv dap`, found on the
   path; Go files and `"type": "go"` in `launch.json`; a `.go` file or folder is built and run, any
   other program is run as it is). A missing server with a known
   recipe **installs itself** the first time a file needs it, in the background, into
   `~/.rustcode/servers` (nothing outside that folder changes; `RUSTCODE_SERVERS_DIR` moves it).
   Three kinds of recipe: packages (`npm`: Python/pyright, TypeScript and JavaScript,
   JSON/HTML/CSS, YAML, Bash, PHP/intelephense, Svelte, Vue, Dockerfile; `go`: gopls; `rustup`:
   rust-analyzer), and **release downloads** for the rest that are not packages (marksman,
   lua-language-server, zls, taplo, texlab), which pick the file for this OS and CPU from the
   newest release of the official GitHub project, fetch it with `curl`, unpack it with `tar`
   (macOS, Linux, Windows x64 and Arm where the project builds them) and keep it, with its
   support files, in `pkg/<name>`. It says so in the status bar and Output log (no pop-ups),
   tells when it is done, and the files that waited are given to it. A recipe is tried once per
   run; if it fails (no `npm`, no network), the reason is in the log.
   `"rustcode.languageServers.autoInstall": false` turns this off.
   - **At install time:** `rustcode --install-language-servers [all|everything|NAME...]` (`-NAME` leaves one
     out) installs them and exits, printing what it does, so installers and scripts can call it;
     `scripts/install-language-servers.sh` and `.ps1` find the program and run it.
     `scripts/bundle-macos.sh release --with-servers` puts them inside `RustCode.app`
     (`Contents/Resources/servers`, without rust-analyzer, which would change the Mac's own
     toolchain); a `servers` folder next to the program works the same on other systems. The
     program searches its own folder after the user's, so a server the user installed wins.
   - **At update time:** when the program's version differs from the one that last looked, the
     servers that were installed are brought to the newest release in the background (a
     release already there is skipped by its tag).
   - **Servers inside VS Code extensions.** Where an extension on open-vsx.org (the registry
     VS Code-compatible editors may use) carries the whole server, its package for this OS and
     CPU is downloaded and the server is taken out of it: **terraform-ls** (from
     `hashicorp.terraform`) and **jdtls** (from `redhat.java`, whose packages also carry the
     **Java runtime**; it becomes `JAVA_HOME` for servers started by RustCode when the user has
     not set one, so Java needs no JDK on the machine). Most other extensions only download
     their server when they run (clangd, Haskell, Kotlin), so there is nothing to take.
   - **Large downloads** (Java about 130 MB, clangd about 100 MB) install the first time a file
     needs them, with their size in the status note, but are not part of `all`; name them
     (`java`, `clangd`) or use `everything`. clojure-lsp (a native binary) and gleam (the
     compiler is the server) are ordinary release downloads.
   - Not covered, because they need their language's own compiler or SDK, which an extension
     does not carry: Haskell (GHC), OCaml (opam), Scala (coursier and a JVM), Dart and Swift (their
     SDKs), Nix, Elixir (Erlang and Elixir), Ruby, PHP's phpactor, and .NET; Kotlin's older
     server cannot read the version of the Java that comes with the Java extension and the
     newer one has no download to fetch; C# extensions on open-vsx.org are under licenses that
     do not allow their parts to be used outside the extension. terraform-ls is not downloaded
     from HashiCorp's own server. For these a log line says how to install them, and
`Language Servers: Install Server for This File...` shows that command and types it into the
   integrated terminal when you accept (the command is the text in backticks in the server's
   `install` hint, so a hint of your own should keep it there). Servers that could not
   start are then tried again. A program that starts and stops at once because it is not really
   installed (a `rustup` shim without its component prints `Unknown binary 'rust-analyzer'`)
   is installed the same way (or reported with the same hint when auto install is off), and is
   started again when its file is closed and opened again after installing.
3. **JavaScript only where needed (in progress, see Phase 2).** The small `vscode` module and
   the Node/Bun process run an extension's commands on demand. Done so far, one main-thread
   service at a time: commands, configuration, and **documents and editors**:
   - RustCode tells the host about its open files (text, language id, version, unsaved or not;
     only what changed, on the 250 ms timer) and which editor has the focus with its selections.
     Extensions see them as `workspace.textDocuments`, `window.activeTextEditor` and
     `visibleTextEditors`, with the events `onDidOpenTextDocument`,
     `onDidChangeTextDocument` (one `contentChange` covering the difference),
     `onDidCloseTextDocument`, `onDidSaveTextDocument`, `onDidChangeActiveTextEditor`,
     `onDidChangeVisibleTextEditors` and `onDidChangeTextEditorSelection`.
   - `TextDocument` has `getText`, `lineAt`, `offsetAt`, `positionAt`, `validateRange/Position`,
     `getWordRangeAtPosition`, `lineCount`, `eol`, `version`, `isDirty`, `languageId` and `save()`.
     Lines and columns count UTF-16 units, as in VS Code. `Position`, `Range`, `Selection`,
     `TextEdit` and `WorkspaceEdit` (text edits) have their usual methods.
   - Extensions change text with `TextEditor.edit`, `workspace.applyEdit`, `registerTextEditorCommand`
     and the editor's `selection(s)` setters. Open files change in the editor as one undo step
     (unsaved); other files change on disk. `TextEditor.edit` is refused (`false`) when the file
     changed since the editor was handed out; the host is shown the new text before it is told
     the result. `window.showTextDocument` opens a file and selects a range.
   - `workspace.openTextDocument` (open files, files on disk, or `{content, language}`),
     `workspace.fs`, `asRelativePath` and `getWorkspaceFolder` work.
   - An extension with `onLanguage:<id>` starts the host when the first file in that language is
     opened (once per run; commands start it too). Extensions that only list `*` or
     `onStartupFinished` still start with the first command.
   - **Several editors:** every editor group is an editor for extensions (`visibleTextEditors`,
     `viewColumn` from 1 in reading order); `showTextDocument` gives the focus to the group that
     already shows a file, and an editor's `selection(s)` can be set in a group that is not
     focused.
   - **Untitled files** are documents named `untitled:<title>` (`isUntitled`): extensions read,
     edit and decorate them like any file. `workspace.registerTextDocumentContentProvider` gives
     text for any other scheme; `openTextDocument(uri)` reads it, and `showTextDocument` shows
     it in an untitled editor with the name of the path (made again if it is already open; it is
     an ordinary untitled file there, not read-only).
   - **Decorations** (`createTextEditorDecorationType`, `setDecorations`, `ThemeColor`):
     `backgroundColor`, `color` and `isWholeLine` are drawn, and
     `renderOptions.after.contentText` is shown after the line beside the blame note. Colors are
     CSS text (`#rgb`, `#rrggbb[aa]`, `rgb()`, `rgba()`, a few names); a `ThemeColor`, borders,
     outlines, gutter icons, the overview ruler and hover messages are accepted and left out.
     Ranges stay where they were put until the extension sets them again.
   - **`TextEditor.insertSnippet`** (with `SnippetString`) uses RustCode's snippet engine: tab
     stops, placeholders, choices and variables. One place gets the snippet: the given range or
     position (the first of several), else the primary selection.
   - **`WorkspaceEdit` file operations** (`createFile` with contents, `deleteFile` to the trash,
     `renameFile`) are made in the order the edit lists them together with its text edits; open
     files follow a rename. The first step that fails stops the rest and `applyEdit` resolves
     `false`.
   - **`onWillSaveTextDocument`** with `event.waitUntil(edits)`: a save by hand (not auto save)
     waits up to 1.4 s for the listeners, makes their edits if the text has not changed, then
     saves. Nothing waits when no extension listens.
   - **Language features (`vscode.languages`).** Extensions can report problems with
     `createDiagnosticCollection` (`set`, `delete`, `clear`, `get`, `has`, `forEach`;
     `getDiagnostics`, `onDidChangeDiagnostics`): they show in Problems and as squiggles, next to
     those of language servers (each source is kept apart and put together per file). Providers
     for **hover, completion, definition, type definition, implementation, references, document
     and range formatting, formatting on type, code actions, signature help, document and
     workspace symbols, rename, folding ranges, inlay hints, document links, semantic tokens and
     linked editing** are asked when the editor asks a language server. Where the answer is a
     list (hover, places, completion, code actions, folding, hints, links) the extension's and
     the server's are put together (a server that does not answer, or answers late, does not hold
     the other back: after three seconds the one that came is given); where there is one right
     answer (formatting, rename, signature, symbols, tokens) the extension's wins. A provider
     that registered the trigger characters (`registerCompletionItemProvider(..., '.', '<')`,
     signature and on-type formatters) is consulted after typing them, as servers' are
     (`completionProvider.triggerCharacters` is read from them now too). A question that a newer
     one about the same file replaces is called off: its `CancellationToken` says so and its answer
     is empty. Code actions that come without changes are resolved at once
     (`resolveCodeAction`). `DocumentSelector`s work with languages, schemes and glob patterns, as
     do `languages.match`, `getLanguages` and `createLanguageStatusItem`. A code action's command
     runs in the extension host with its arguments. Answers are turned into LSP-shaped JSON in the
     host and read by the converters language servers use. Code lens, document highlight, color,
     also **call hierarchy and type hierarchy** (listed in the same palette), **document
     highlights, code lenses, colors, selection ranges and inline completions** (below), and
     ranged semantic tokens (a provider of ranges is asked for the whole file). Providers for
     inline values, evaluatable expressions, declarations and drop or paste edits are accepted so
     that an extension loads, and not asked: the first two belong to the debugger's views, which
     do not take them, and the others have nothing in the editor to ask them.
   - **Completion documentation and resolve.** A suggestion carries its documentation and the other
     changes taking it makes (`additionalTextEdits`, an `import`); the one that is highlighted is
     asked for what it came without (`completionItem/resolve` from a server, `resolveCompletionItem`
     from the extension that gave it, once each). The documentation is drawn as Markdown in a
     panel beside the list. Taking a suggestion makes its edits first, then types it where the
     caret was; one taken before its rest arrived gets its edits when they come, if they are above
     the caret.
   - **Document highlights.** Once the caret has rested on a word, the other uses of the symbol in
     the file are marked (from `textDocument/documentHighlight` or an extension's
     `registerDocumentHighlightProvider`); the marks go when the caret leaves them or the text
     changes.
   - **Code lenses** (`textDocument/codeLens`, `registerCodeLensProvider`): drawn at the end of
     their line, after the blame note (the editor has no room above a line), and clicking one runs
     its command: on the server for a server's, in the extension host for an extension's.
     Lenses that come without a command are resolved (`codeLens/resolve`, the first 30); an
     extension's are resolved in the host.
   - **Colors** (`textDocument/documentColor`, `registerColorProvider`): a square in the color
     stands before each color written in the text (not in lines that wrap); clicking it, or
     `Pick Color at the Caret...`, opens a picker with red, green, blue and alpha sliders over
     gradients, a preview, and the ways the provider can write the color (`#rrggbb`, `rgb()`,
     `hsl()`, with the provider's own edits). Choosing one writes it into the text; Escape or a
     click elsewhere closes the picker, and it will not write into text that changed.
   - **Expand Selection and Shrink Selection** (`ctrl-shift-cmd-right` and `-left` on a Mac,
     `shift-alt-right` and `-left` elsewhere): the selection grows to the next range around it. A
     language server's (`textDocument/selectionRange`) or an extension's
     (`registerSelectionRangeProvider`) nested ranges are used together with the built-in word,
     line, line with its break and whole file; shrinking goes back down, to the caret last.
   - **Inline completions** (`registerInlineCompletionItemProvider`, which the extensions of AI
     assistants use): once typing has paused for half a second, the extension is asked, and the
     first suggestion shows dimly after the caret (the first line, with a sign when there are
     more). Tab types it (also replacing the range it names); typing, moving or Escape sends it
     away, and Tab is then an ordinary tab. Language servers are not asked.
   - **Type hierarchy:** `Show Type Hierarchy: Supertypes` and `Subtypes`, from a server
     (`typeHierarchy/supertypes`) or an extension.
   - **Questions from extensions** (`window.showQuickPick`, `showInputBox`, `createQuickPick`,
     `createInputBox`, `showInformationMessage` and its kin, `showOpenDialog`, `showSaveDialog`,
     `withProgress`): a pick list or a line is asked in the command palette (several can be
     checked in a list, with a first row that accepts them; a password shows as dots;
     `validateInput` can refuse what was typed, and the box stays up with the reason); a message
     with buttons is a notice that stays up for two minutes with the buttons in it, and
     `{ modal: true }` makes it a dialog over the window; closing, escaping or waiting too long
     answers `undefined`; files are chosen in the system's own dialog; progress in a notification
     is a notice that changes and goes when the task is done. A quick pick made with
     `createQuickPick()` is asked when shown: what is typed is not reported while it is typed, and
     its items cannot change while it is up.
   - **Real extensions run.** `crates/infrastructure/tests/real_extensions.rs` (ignored; see its
     header) runs the real **Prettier** (`esbenp.prettier-vscode` 12.4: formats a file, shows its
     status bar item) and **ESLint** (`dbaeumer.vscode-eslint` 3.0, which starts its server over
     IPC through `vscode-languageclient`: reports problems from `eslint` 9, offers seven quick
     fixes, and runs the fix command, which has the server ask for an edit) in the extension host.
     What they needed, and is there now: ES-module extensions (`import ... from 'vscode'`),
     `workspace.isTrusted`, section values as members of `getConfiguration()`, `inspect` that
     tells a default from the user's own value, VS Code's type-based defaults for settings with
     none, status bar items and language status items, `withProgress`, log output channels, file
     system watchers, `RelativePattern`, `WorkspaceEdit.set` with annotated edits, the data
     classes of the API (code lens, symbols, folding, inlay hints, semantic tokens, tree items,
     errors) and the enums, any other class of the API as a stub that can be extended and fails
     when used, events that nothing here raises as events nobody hears, and a recent API version
     (1.105). Extensions that ask for `*` or `onStartupFinished` now start with the program.
   - Hover popups draw the server's Markdown: code blocks in the code font with syntax colors,
     headings, bold, italics, inline code, lists, quotes and rules. A long answer is whole and
     scrolls inside the popup (it is not cut at 14 lines), the pointer can move onto it, and it
     opens upwards when there is more room above.
   - **Views, source control, tasks and terminals** (checked with a real extension in the app):
     - *Tree views* (`createTreeView`, `registerTreeDataProvider`): a container in the activity
       bar (`viewsContainers`) or a section of the Explorer or Source Control view (`views`).
       Rows open and close, show icons (`ThemeIcon` with a `ThemeColor`, or a file) and
       descriptions, run the item's command on a click (the element comes back to the command
       as the very object), and have a right-click menu (`view/item/context`) and buttons in the
       header (`view/title`), both filtered by their `when` clauses. `onDidChangeTreeData`,
       `reveal`, title, message and badge work; the extension starts when its view is first shown.
     - *Source control* (`scm.createSourceControl`): a section beside git (also in folders with
       no repository) with its count, commit box (typed in the palette's box; the extension's
       accept command is a button), resource groups, struck-through and faded files, a click that
       runs the resource's command, and the menus `scm/title`, `scm/resourceGroup/context` and
       `scm/resourceState/context`. Not done: quick diff (change marks in the gutter from a
       provider), `statusBarCommands` in the status bar, input validation.
     - *Tasks* (`tasks.registerTaskProvider`, `fetchTasks`, `executeTask`, `ShellExecution`,
       `ProcessExecution`, `CustomExecution`): `Tasks: Run Task`, `Run Build Task`, `Run Test
       Task` list them with those of `.vscode/tasks.json`; a run is a command line in a named
       terminal tab, and the extension hears how it ended (`onDidStartTask`, `onDidEndTask`).
       Not done: problem matchers (output is not turned into problems), `dependsOn`.
     - *Terminals* (`window.createTerminal`, `sendText`, `show`, `dispose`, `terminals`,
       `onDidOpenTerminal`, `onDidCloseTerminal`, `onDidChangeActiveTerminal`, and a
       `Pseudoterminal` the extension draws and reads), link providers
       (`registerTerminalLinkProvider`: the line under the mouse is asked for, the link is
       underlined, and a modifier click calls `handleTerminalLink`), and shell integration
       (`terminal.shellIntegration`, `executeCommand`, `onDidStartTerminalShellExecution`,
       `onDidEndTerminalShellExecution` with `read()`, `onDidChangeTerminalShellIntegration`).
       Shell integration works for `zsh`, `bash` and `fish` (the shell is started with a small script
       that prints VS Code's `OSC 633` marks; the user's own startup files still run) in every
       terminal, the user's too; turn it off with `terminal.integrated.shellIntegration.enabled`.
       Not done: shell integration for PowerShell and `cmd`, a shell path other than the
       user's (it is started with `exec`), and terminal profiles.
   - **Editors:** borders and outlines of decorations are drawn as frames (colors, width,
     dashes, round corners); `ThemeColor`s are looked up in the theme in use; `insertSnippet`
     with several locations fills all of them in together; `onWillSaveTextDocument` is also
     asked on auto save, with the reason (`TextDocumentSaveReason`: manual, after delay); text
     from a `TextDocumentContentProvider` follows `onDidChange` while it is open, and such
     documents are read-only. Not done: notebooks of extensions (`registerNotebookSerializer`).
   - **Webviews** (`extension_services.js`, `webview_tabs.rs`, `webview_native.rs`): the page is
     shown by the system webview (`wry`; WebKit on macOS, WebView2 on Windows), a child view put
     over the room its tab leaves. Done: `createWebviewPanel` (in a column, beside, or in the
     active group; title, `reveal`, `dispose`, `onDidDispose`, `onDidChangeViewState`),
     `webview.html`, `postMessage` both ways, `acquireVsCodeApi` (`postMessage`, `getState`,
     `setState`, kept when the HTML is set again), `asWebviewUri` and `cspSource` (files are
     served only from `localResourceRoots`, or from the workspace and the extensions' folders
     when none are named), `enableScripts` and `enableCommandUris` (`command:` links), the
     `--vscode-*` colors and fonts of the theme in use with `vscode-dark` / `vscode-light` on
     the body (they follow a theme change), shortcuts the page does not use reaching the editor,
     external links opening in the browser; custom editors (`contributes.customEditors`,
     `CustomTextEditorProvider` and `CustomEditorProvider` / `CustomReadonlyEditorProvider`:
     files open in them by default, "Reopen Editor With" picks another, save and the unsaved
     mark and undo/redo work); views of the side bar drawn with a page
     (`registerWebviewViewProvider`, with title, description and badge); panels come back after a
     restart when their extension registered a serializer (`registerWebviewPanelSerializer`; the
     page's `setState` is what is given back); typed arrays and buffers go through
     `postMessage` as bytes; the tab shows the page's `iconPath`. Not done: pages on Linux (they
     say so; it needs GTK's WebKit and cannot be built or tried from here),
     `retainContextWhenHidden` (a page is always kept), the same file in a text tab and a custom
     editor tab at once (a document is shown by one or the other), and custom editor backups
     (the editor keeps no unsaved work for any file, so there is nothing to hand them). When
     the window draws something over a page (the palette, a menu, a toast, a dialog), the page
     is replaced by a picture of itself, which the window draws under them and which gets the
     clicks; this is done on macOS, where WebKit can make the picture; elsewhere the page is
     hidden while something is over it.
   - **Also done:** the gutter marks of an extension's source control provider
     (`quickDiffProvider`) when git has nothing for the file; `statusBarCommands` of a provider in
     the status bar; `inputBox.validateInput` shown under the commit box; problem matchers (`$tsc`,
     `$gcc`, `$go`, `$rustc`, `$eslint-compact`, `$eslint-stylish`, `$msCompile`, `$python`,
     the extensions' own `contributes.problemMatchers`, and `problemMatcher` objects of tasks)
     turn the output of a task into problems; `tasks.json` groups, default tasks (`Run Build Task`
     runs the default at once), `dependsOn` and `dependsOrder`; terminals of extensions start the
     shell they asked for, with their environment.
   - **Survey of the 80 most downloaded Open VSX extensions** (`scripts/extension-survey`): each
     is started alone in the host. At the start 9 activated; after the compatibility layer
     (`extension_compat.js`: `l10n`, `extensions.getExtension`, the full extension context,
     `workspace.fs`, `findFiles`, the editor's own commands, classes and enumerations of the
     parts of the API RustCode has no window for, registrations that are accepted and never
     used) 59 of the 75 that have code activate without an error, and Dart-Code with its Flutter
     extension together. What is left fails for what the machine lacks (Java 25, R, a native
     module, a download) or needs a page of its own. That is starting, not working: the features
     that need webviews, notebooks, tests, chat or a sign-in are not there, and rust-analyzer and
     the Python extension start but also need their own programs. Checked beyond starting: the
     YAML extension reports a real syntax error (`real_extensions.rs`), as Prettier and ESLint do.
   Not done, and not planned soon: running VS Code's own extension host code with its RPC
   protocol (`extHost.protocol.ts`) in Rust. It is a different design, with every API split into
   a main-thread and an extension-host half over one RPC channel; it would make each of the
   services above come for free, but it is months of work and this host reaches the same
   extensions one service at a time.
4. **Webviews** (done, see layer 3 above; Phase 5 keeps the Markdown preview and notebooks).

- Test order for layer 3: themes, snippets, simple language extensions (layer 1 covers these),
  then ESLint, Prettier, GitLens, rust-analyzer.
- Success: the top 50 Open VSX extensions work.

## Phase 5: Webviews and custom UI (done, with the limits written below)

- System webview (`wry`) for webview panels and custom editors: **done**, see Phase 4, layer 3.
- `postMessage` bridge between webview and extension host: **done**.
- Markdown preview: **done** (side by side, in a panel, or in place of the editor; scroll sync).
- Notebooks: **done, with limits.** `.ipynb` (nbformat 4) opens as cells, with Markdown rendered,
  code colored, outputs shown (text, values, tracebacks, pictures, SVG, Markdown, JSON; HTML as
  its plain text with "Open as page", which shows the markup, scripts included, in a tab),
  command and edit modes, `shift-enter` / `ctrl-enter` / `alt-enter` to run, drag-select in a
  cell, cells dragged by their number to move them, `z` / `shift-z` to walk back and redo
  changes of the list of cells (deleted cells come back, moves, kinds, clear outputs), and a
  variables view (toolbar) that follows every run. Python notebooks run on the built-in kernel
  (a child `python3` that keeps its variables; interrupt and restart work). Any other kernel
  (R, Julia, an environment's own Python kernel named in the notebook) runs through
  `jupyter_client` when it and the kernel are installed (`jupyter_bridge.py`; variables for
  Python kernels). Not done: ipywidgets and other comm-based outputs (the comm protocol is not
  spoken), HTML and JavaScript drawn inside the cell instead of in a tab, drag-select across
  cells, and variables for kernels that are not Python.
- Flutter and Dart: a Dart grammar, snippets for widgets, `dart` found through the Flutter SDK.
  Above every Dart file in a package there is a bar: `Run App` and `Debug App` (`Run` and
  `Debug` for plain Dart), the device (click to pick; also at the right of the status bar), and,
  while an app runs, `Hot Reload`, `Hot Restart` and `Stop`. `Run | Debug` also shows after the
  `main` line. Run starts `flutter run` in a terminal tab (saving a `.dart` file hot reloads it);
  Debug goes through `flutter debug_adapter` / `dart debug_adapter` with breakpoints, stepping,
  variables and hot reload. Palette commands: Run, Debug, Select Device, Hot Reload/Restart,
  Stop, Get/Upgrade Packages, Clean, Doctor, Analyze, Run Tests, New Project. `Flutter: Open DevTools` (and a
  `DevTools` button while an app runs) shows Flutter DevTools, with the widget inspector, in a
  tab: at the address `flutter run` printed, or from a `dart devtools` server it starts in its
  own terminal tab, connected to the app. Not done: an inspector drawn inside the editor, and the
  lens drawn above `main` (it is drawn at the end of the line, as the editor has no rows of
  their own for lenses).

## Phase 6: Remote and collaboration (optional)

- Remote SSH and dev containers. VS Code's remote server protocol is proprietary, so this
  needs our own protocol.
- Live Share has no open equivalent.

## Phase 7: Hardening (ongoing)

Done:

- **Large files.** An 80 MB, 1M-line Rust file opens in about 4 s and settles at 470-620 MB
  resident with ~0% idle CPU. Files over 20,000 lines are colored lazily: only the lines in
  view plus 400 below, never past line 100,000 (`docs.rs`, `IncrementalHighlighter::advance_to`).
  The status bar samples the first 500 lines for the line ending instead of scanning the text.
  The domain-layer benchmark is `crates/domain/tests/large_file.rs`.
- **Settings page** (`preferences.openSettings`): every setting in `setting_catalog.rs`, grouped by
  category, searchable, with a control per kind (switch, number, text, choice, JSON). Edits are
  written to `settings.json` in place, keeping comments and layout.
- **Keyboard shortcuts page** (`preferences.openShortcuts`): every command with its binding,
  searchable; click a row to record a new key, reset to the default, or remove a binding.
  Changes are written to `keybindings.json` (VS Code key format, `-command` removals).
- **Icons**: tabs of the settings and shortcuts pages, a gear at the bottom of the activity bar
  (menu: command palette, settings, keyboard shortcuts, themes, the JSON files), command palette
  rows, panel tabs and the start screen, which also lists Settings and Keyboard Shortcuts.
- **Menus** regrouped with separators, an icon column and the shortcut of each command; the
  explorer's empty space has its own right-click menu (new file/folder, open folder, refresh).
- **Status bar**: position (go to line), indentation, line ending, language and theme are
  buttons with a picker each.
- **Extension survey** (`scripts/extension-survey/`): the 499 most downloaded Open VSX
  extensions, each started alone in the host for seven seconds. 105 are data only (themes,
  snippets, grammars: nothing to run), 296 of the 394 with code activated cleanly, and 98 logged an
  error. Most of those are the machine or a missing neighbour, not the host: no Java 21/25, no R,
  no `dbclient-jdbc` or Salesforce services extension, native `node-pty`, unsigned or
  non-executable bundled binaries (the survey's unzip drops the execute bit), a server that needs
  its own `typescript` folder. The gaps in the host itself were fixed: `inspect()` of a setting
  nobody set now returns the usual object, `getConfiguration()` without a section exposes every
  section, a declared type without a default reads as the empty value of the type, the editor's
  `search.exclude`/`files.exclude` defaults exist, and `extensions.allAcrossExtensionHosts` exists.
  A second round added: a stand-in for VS Code's built-in `vscode.git` extension (git's path and
  state, no repositories), `contributes.configurationDefaults` (the `[language]` blocks), a
  `SlowBuffer` shim for Node 25+, and unhandled promise rejections go to the log instead of a
  toast, as in VS Code. After both rounds 308 of 394 activate cleanly and 86 log an error (it was
  98). Left over, all of them needing something that is not here: Vue.volar (VS Code's built-in
  TypeScript extension), csharp (the .NET Install Tool extension, which must be installed with
  it), vim (logs an unhandled read of its own first-run `.registers` file).

Not done:

- Accessibility (AccessKit): GPUI 0.2.2 has no AccessKit integration, so the custom renderer is
  invisible to screen readers. This needs work in GPUI itself.
- Packaging and automatic updates for Windows and Linux (nothing can be run or checked for them
  from macOS).

Done for macOS:

- **Auto-update** from GitHub releases (`GithubUpdates`): checked at start (`rustcode.update.check`),
  or by "Check for Updates..."; a title bar button downloads, then restarts into the new app.
  `rustcode.update.repository` (or `RUSTCODE_UPDATE_REPO` at build time) names the repository.
  `scripts/bundle-macos.sh release --dist` makes the zip, `.sha256` and `.dmg` to upload, and
  signs/notarizes with `CODESIGN_IDENTITY` / `NOTARY_PROFILE`.
- **Telemetry** is opt-in (`rustcode.telemetry.enabled`, off by default) and local: event names go to
  `telemetry.jsonl` in the data folder; nothing is sent over the network.

## Known limits

Last updated after the Phase 1 hardening pass.

Tested headlessly (GPUI test platform): click, drag, double/triple-click, alt-click, tab column
mapping, IME composition and UTF-16 ranges, theme cycling, find-box click and history.

Tested with the real TextMate engine: a 20k-line Rust file tokenizes in bounded slices, and a
block-comment edit on line 0 restyles the last line and restores it on undo
(`crates/infrastructure/tests/highlight_load.rs`).

Checked on screen (macOS screenshots of the running app, clicks driven with `cliclick`):
gutter, title bar, syntax colors, caret, tab indentation, CJK and emoji drawing at the right
width, click and drag over wide characters, double-click word, triple-click line, find box
(open, click-to-place caret, typing, escape to close), the command palette and theme picker (open, filter, click a row to switch theme), and tabs
(new file, open by path, switch by click, close dirty tab with confirmation) with the status bar.

Still needs a human:

- **Windows executable icon.** `RustCode.ico` is generated, but embedding it in the `.exe` needs a
  Windows build (a resource compiler), which has not been set up.
- **Windows and Linux.** Nothing was run there, and the Windows target does not cross-compile
  from macOS (a C dependency needs the MSVC headers). The title bar with window controls, the
  `secondary` bindings and the per-platform movement keys are only exercised on macOS, where
  `--chrome windows` previews the layout.
- **Real IME** (macOS Pinyin, Kana) and dead keys. Composition logic is tested through the
  handler API only; synthetic key events cannot start a real composition.

Not fixable now:

- **Keyboard navigation of the macOS system menu.** The system menu bar handles it (Ctrl+F2 with
  full keyboard access turned on in System Settings). For mnemonics and Alt focus, switch on the
  menu bar in the window from the command palette.
- **Semantic token colors.** They need a language server, which is Phase 3.
- **Theme list.** The picker lists the `--theme` file (or Dark+), Light+ and the themes of
  installed extensions (Phase 4, layer 1), which are read at start-up.
