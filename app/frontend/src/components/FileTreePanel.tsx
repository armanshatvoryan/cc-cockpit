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
  ftHideIgnored,
  ftRootEntries,
  ftToggleExpand,
  ftToggleHidden,
  ftToggleHideIgnored,
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
  ftCdActivePane,
  ftHome,
  ftRecents,
  ftRepos,
  ftLoadRepos,
} from "../store";

/** Clickable breadcrumb segments for `root`, each carrying the cumulative abs
 *  path it cd's to. Rooted below `$HOME` when `root` is under it (so labels read
 *  `Workflows › cc-cockpit › app`, not the full `/Users/…` chain); otherwise
 *  absolute from `/`. */
function crumbSegs(root: string, home: string): { label: string; path: string }[] {
  if (!root) return [];
  let rel = root;
  let base = "";
  if (home && (root === home || root.startsWith(home + "/"))) {
    rel = root.slice(home.length); // "" (at home) or "/a/b"
    base = home;
  }
  const segs: { label: string; path: string }[] = [];
  let acc = base;
  for (const p of rel.split("/").filter(Boolean)) {
    acc = acc + "/" + p;
    segs.push({ label: p, path: acc });
  }
  return segs;
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
        onDblClick={() =>
          e.isDir ? void ftCdActivePane(e.path) : void ftInsertIntoActivePane(e)
        }
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

/** Last path segment (for recent-root labels). */
function baseNameOf(p: string): string {
  const parts = p.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || p;
}

/** Clickable breadcrumb: Home + (… if truncated) + the last 3 segments. Each
 *  click cd's the active pane to that ancestor. Width-bounded — deeper jumps go
 *  through the repo picker / recents. */
const Breadcrumb: Component = () => {
  const segs = () => crumbSegs(fileTree.root, ftHome());
  const shown = () => segs().slice(-3);
  const truncated = () => segs().length > shown().length;
  const homePath = () => ftHome() || "/";
  return (
    <div class="ft-breadcrumb" title={fileTree.root}>
      <button
        class="ft-crumb ft-crumb-home"
        title={`cd ${homePath()}`}
        onClick={() => void ftCdActivePane(homePath())}
      >
        Home
      </button>
      <Show when={truncated()}>
        <span class="ft-crumb-sep">›</span>
        <span class="ft-crumb-ellipsis">…</span>
      </Show>
      <For each={shown()}>
        {(seg) => (
          <>
            <span class="ft-crumb-sep">›</span>
            <button
              class="ft-crumb"
              title={`cd ${seg.path}`}
              onClick={() => void ftCdActivePane(seg.path)}
            >
              {seg.label}
            </button>
          </>
        )}
      </For>
    </div>
  );
};

/** Header repo-picker: a dropdown of recently-visited roots + sibling project
 *  dirs (discovered when it opens). A pick cd's the active pane there. Filled
 *  dot = a git repo, hollow = a plain project dir. */
const RepoPicker: Component = () => {
  const [open, setOpen] = createSignal(false);
  const toggle = () => {
    const next = !open();
    setOpen(next);
    if (next) void ftLoadRepos();
  };
  const pick = (path: string) => {
    setOpen(false);
    void ftCdActivePane(path);
  };
  return (
    <div class="ft-repo">
      <button
        class="ft-icon-btn ft-repo-btn"
        classList={{ "ft-icon-on": open() }}
        title="Jump to a repo"
        aria-label="Repo picker"
        onClick={toggle}
      >
        ▾ repos
      </button>
      <Show when={open()}>
        <div class="ft-menu-backdrop" onClick={() => setOpen(false)} />
        <div class="ft-repo-menu">
          <Show when={ftRecents().length > 0}>
            <div class="ft-repo-section">recent</div>
            <For each={ftRecents()}>
              {(p) => (
                <button class="ft-menu-item ft-repo-item" title={p} onClick={() => pick(p)}>
                  <span class="ft-repo-name">{baseNameOf(p)}</span>
                  <span class="ft-repo-path">{p}</span>
                </button>
              )}
            </For>
            <div class="ft-menu-sep" />
          </Show>
          <div class="ft-repo-section">repos</div>
          <Show
            when={ftRepos().length > 0}
            fallback={<div class="ft-menu-empty">no sibling repos</div>}
          >
            <For each={ftRepos()}>
              {(r) => (
                <button class="ft-menu-item ft-repo-item" title={r.path} onClick={() => pick(r.path)}>
                  <span
                    class="ft-repo-dot"
                    classList={{ "ft-repo-dot-on": r.isRepo }}
                  >
                    {r.isRepo ? "●" : "○"}
                  </span>
                  <span class="ft-repo-name">{r.name}</span>
                </button>
              )}
            </For>
          </Show>
        </div>
      </Show>
    </div>
  );
};

export const FileTreePanel: Component = () => {
  return (
    <aside class="ft-panel" aria-label="File tree">
      <header class="ft-header">
        <span class="ft-title">FILES</span>
        <RepoPicker />
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
        <button
          class="ft-icon-btn"
          classList={{ "ft-icon-on": ftHideIgnored() }}
          title={ftHideIgnored() ? "Show .gitignored" : "Hide .gitignored"}
          aria-label="Toggle gitignored"
          onClick={() => ftToggleHideIgnored()}
        >
          ⊘
        </button>
        <button class="ft-icon-btn" title="Refresh" aria-label="Refresh" onClick={() => ftRefresh()}>
          ⟳
        </button>
      </header>

      <Breadcrumb />

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
