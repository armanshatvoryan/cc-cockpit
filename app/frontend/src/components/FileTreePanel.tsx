// FileTreePanel (v1.1) — a DOCKED left-sidebar file browser.
//
// Not an editor: the tree is a navigation + path helper for the terminals and
// agents. It FOLLOWS the active pane's cwd (the store probes `pane_current_path`
// and re-roots here). Lazy: each folder lists its children on first expand.
//
// Interactions (Phase C): single-click browses (folder expand / file select),
// DOUBLE-click a file inserts its path into the active pane, RIGHT-click opens
// the context menu (Open in Terminal, Reveal, New File/Folder, Copy Path(/Rel),
// Attach to Agent, Delete→Trash). ⌘B toggles the whole sidebar. Header: + File,
// + Folder, ⟳ refresh, ⚙ show-dotfiles.

import { For, Show, createSignal, onMount, type Component } from "solid-js";
import type { FileEntry } from "../ipc";
import {
  fileTree,
  ftExpanded,
  ftShowHidden,
  ftRootEntries,
  ftToggleExpand,
  ftToggleHidden,
  ftRefresh,
  ftInsertIntoActivePane,
  ftOpenMenu,
  ftMenu,
  ftCloseMenu,
  ftOpenInTerminal,
  ftRevealInFinder,
  ftCopyPath,
  ftCopyRelPath,
  ftBeginNew,
  ftNewEntry,
  ftCommitNew,
  ftCancelNew,
  ftRequestDelete,
  ftPendingDelete,
  ftCancelDelete,
  ftConfirmDelete,
  ftLiveAgents,
  ftAttachToAgent,
} from "../store";

/** Breadcrumb segments from the root path: `Home › … › <last two dirs>`. */
function crumbs(root: string): string[] {
  if (!root) return [];
  return root.split("/").filter(Boolean).slice(-2);
}

/** The dir a new-entry / context action targets: a folder itself, else a file's
 *  parent dir. */
function dirOf(entry: FileEntry): string {
  if (entry.isDir) return entry.path;
  const t = entry.path.replace(/\/+$/, "");
  const i = t.lastIndexOf("/");
  return i <= 0 ? "/" : t.slice(0, i);
}

/** Inline input row for New File / New Folder (Enter commits, Esc/blur cancels). */
const NewEntryRow: Component<{ depth: number; isDir: boolean }> = (props) => {
  let inputEl!: HTMLInputElement;
  onMount(() => inputEl.focus());
  return (
    <div class="ft-row ft-newrow" style={{ "padding-left": `${props.depth * 12 + 8}px` }}>
      <span class="ft-twist" />
      <span class="ft-glyph">{props.isDir ? "▒" : "·"}</span>
      <input
        ref={inputEl}
        class="ft-new-input"
        placeholder={props.isDir ? "folder name" : "file name"}
        spellcheck={false}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            void ftCommitNew(inputEl.value);
          } else if (e.key === "Escape") {
            e.preventDefault();
            ftCancelNew();
          }
        }}
        onBlur={() => ftCancelNew()}
      />
    </div>
  );
};

const TreeNode: Component<{ entry: FileEntry; depth: number }> = (props) => {
  const e = props.entry;
  const open = () => !!ftExpanded[e.path];
  const children = () => fileTree.entries[e.path] ?? [];
  const loading = () => !!fileTree.loading[e.path];
  const newHere = () => ftNewEntry()?.parent === e.path;
  return (
    <div class="ft-node">
      <div
        class="ft-row"
        classList={{ "ft-row-dir": e.isDir }}
        style={{ "padding-left": `${props.depth * 12 + 8}px` }}
        title={e.path}
        onClick={() => e.isDir && ftToggleExpand(e.path)}
        onDblClick={() => void ftInsertIntoActivePane(e)}
        onContextMenu={(ev) => {
          ev.preventDefault();
          ftOpenMenu(e, ev.clientX, ev.clientY);
        }}
      >
        <span class="ft-twist">
          {e.isDir ? (loading() ? "·" : open() ? "▾" : "▸") : ""}
        </span>
        <span class="ft-glyph">{e.isDir ? "▒" : "·"}</span>
        <span class="ft-name">{e.name}</span>
      </div>
      <Show when={e.isDir && open()}>
        <Show when={newHere()}>
          <NewEntryRow depth={props.depth + 1} isDir={ftNewEntry()!.isDir} />
        </Show>
        <For each={children()}>
          {(c) => <TreeNode entry={c} depth={props.depth + 1} />}
        </For>
      </Show>
    </div>
  );
};

const ContextMenu: Component = () => {
  const m = ftMenu()!;
  const e = m.entry;
  const [attachOpen, setAttachOpen] = createSignal(false);
  const run = (fn: () => void) => () => {
    fn();
    ftCloseMenu();
  };
  return (
    <>
      <div class="ft-menu-backdrop" onClick={() => ftCloseMenu()} onContextMenu={(ev) => { ev.preventDefault(); ftCloseMenu(); }} />
      <div class="ft-menu" style={{ left: `${m.x}px`, top: `${m.y}px` }}>
        <button class="ft-menu-item" onClick={run(() => void ftOpenInTerminal(e))}>
          Open in Terminal
        </button>
        <button class="ft-menu-item" onClick={run(() => void ftRevealInFinder(e.path))}>
          Reveal in Finder
        </button>
        <div class="ft-menu-sep" />
        <button class="ft-menu-item" onClick={run(() => ftBeginNew(false, dirOf(e)))}>
          New File
        </button>
        <button class="ft-menu-item" onClick={run(() => ftBeginNew(true, dirOf(e)))}>
          New Folder
        </button>
        <div class="ft-menu-sep" />
        <button class="ft-menu-item" onClick={run(() => ftCopyPath(e.path))}>
          Copy Path
        </button>
        <button class="ft-menu-item" onClick={run(() => ftCopyRelPath(e.path))}>
          Copy Relative Path
        </button>
        <div class="ft-menu-sep" />
        <button
          class="ft-menu-item ft-menu-sub"
          onClick={() => setAttachOpen((v) => !v)}
        >
          Attach to Agent <span class="ft-menu-caret">{attachOpen() ? "▾" : "▸"}</span>
        </button>
        <Show when={attachOpen()}>
          <Show
            when={ftLiveAgents().length > 0}
            fallback={<div class="ft-menu-empty">no live agents</div>}
          >
            <For each={ftLiveAgents()}>
              {(a) => (
                <button
                  class="ft-menu-item ft-menu-agent"
                  onClick={run(() => void ftAttachToAgent(e, a.paneId))}
                >
                  {a.label}
                </button>
              )}
            </For>
          </Show>
        </Show>
        <div class="ft-menu-sep" />
        <button
          class="ft-menu-item ft-menu-danger"
          onClick={run(() => ftRequestDelete(e))}
        >
          Delete
        </button>
      </div>
    </>
  );
};

const DeleteModal: Component = () => {
  const e = ftPendingDelete()!;
  return (
    <div class="ft-modal-backdrop" onClick={() => ftCancelDelete()}>
      <div class="ft-modal" onClick={(ev) => ev.stopPropagation()}>
        <div class="ft-modal-title">Move to Trash?</div>
        <div class="ft-modal-body">
          <span class="ft-modal-name">{e.name}</span> will be moved to the macOS
          Trash (recoverable from Finder).
        </div>
        <div class="ft-modal-actions">
          <button class="btn" onClick={() => ftCancelDelete()}>
            Cancel
          </button>
          <button class="btn btn-danger" onClick={() => void ftConfirmDelete()}>
            Trash
          </button>
        </div>
      </div>
    </div>
  );
};

export const FileTreePanel: Component = () => {
  return (
    <aside class="ft-panel" aria-label="File tree">
      <header class="ft-header">
        <span class="ft-title">FILES</span>
        <span class="ft-spacer" />
        <button class="ft-icon-btn" title="New File" aria-label="New file" onClick={() => ftBeginNew(false)}>
          ＋
        </button>
        <button class="ft-icon-btn" title="New Folder" aria-label="New folder" onClick={() => ftBeginNew(true)}>
          ＋▒
        </button>
        <button
          class="ft-icon-btn"
          classList={{ "ft-icon-on": ftShowHidden() }}
          title={ftShowHidden() ? "Hide dotfiles" : "Show dotfiles"}
          aria-label="Toggle dotfiles"
          onClick={() => ftToggleHidden()}
        >
          ⚙
        </button>
        <button class="ft-icon-btn" title="Refresh" aria-label="Refresh" onClick={() => ftRefresh()}>
          ⟳
        </button>
      </header>

      <div class="ft-breadcrumb" title={fileTree.root}>
        <span class="ft-crumb-home">Home</span>
        <For each={crumbs(fileTree.root)}>
          {(seg) => (
            <>
              <span class="ft-crumb-sep">›</span>
              <span class="ft-crumb">{seg}</span>
            </>
          )}
        </For>
      </div>

      <div class="ft-body">
        <Show when={ftNewEntry()?.parent === fileTree.root}>
          <NewEntryRow depth={0} isDir={ftNewEntry()!.isDir} />
        </Show>
        <Show
          when={!fileTree.error}
          fallback={<div class="ft-empty ft-error">{fileTree.error}</div>}
        >
          <Show
            when={ftRootEntries().length > 0 || ftNewEntry()?.parent === fileTree.root}
            fallback={
              <div class="ft-empty">{fileTree.root ? "empty" : "no active pane"}</div>
            }
          >
            <For each={ftRootEntries()}>
              {(e) => <TreeNode entry={e} depth={0} />}
            </For>
          </Show>
        </Show>
      </div>

      <footer class="ft-footer">follows active pane · ⌘B to hide</footer>

      <Show when={ftMenu()}>
        <ContextMenu />
      </Show>
      <Show when={ftPendingDelete()}>
        <DeleteModal />
      </Show>
    </aside>
  );
};
