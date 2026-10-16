<script>
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { size, when } from './lib/format.js';

  let { onBack, onOpen } = $props();

  let overview = $state(null);
  let error = $state('');
  /** The cleanup's settings as edited here (the saved ones until changed). */
  let draft = $state(null);
  /** A dry run for the edited settings is on its way. */
  let pending = $state(false);
  let busy = $state(false);
  /** Waiting for "Turn on" to be confirmed. */
  let confirming = $state(false);
  let message = $state('');
  /** The Outplayed folder to import, what a look at it found, and what's going on. */
  let folder = $state('');
  let survey = $state(null);
  let surveying = $state(false);
  let importing = $state(false);
  let imported = $state('');
  /** An import waiting for "Allow" before the cleanup may remove its videos. */
  let allowing = $state(null);

  const plan = $derived(overview?.plan);
  const saved = $derived(overview?.settings);
  const valid = $derived(
    draft != null &&
      Number.isInteger(draft.keepDays) &&
      draft.keepDays >= 0 &&
      Number.isFinite(draft.fullMatchesGb) &&
      draft.fullMatchesGb >= 0 &&
      Number.isFinite(draft.clipsWarnGb) &&
      draft.clipsWarnGb >= 0,
  );
  const limitsOf = (s) =>
    s && JSON.stringify([s.keepDays, s.fullMatchesGb, s.clipsWarnGb]);
  const edited = $derived(draft != null && saved != null && limitsOf(draft) !== limitsOf(saved));
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

  /** The limits the dry run on screen was made with (not reactive). */
  let shownKey = null;
  /** Each load's number: only the latest one's answer is shown. */
  let latest = 0;

  /** Brings the view up to date: the dry run under the edited limits, if valid. */
  async function load() {
    const ticket = ++latest;
    const useDraft = edited && valid;
    try {
      const next = await invoke('storage_overview', {
        draft: useDraft ? { ...$state.snapshot(draft), cleanup: false } : null,
      });
      if (ticket !== latest) return;
      overview = next;
      if (draft == null) draft = { ...next.settings };
      if (!folder) folder = next.imports[0]?.root ?? next.outplayedFolder ?? '';
      shownKey = limitsOf(useDraft ? draft : next.settings);
      error = '';
    } catch (e) {
      if (ticket === latest) error = String(e);
    } finally {
      if (ticket === latest) pending = false;
    }
  }

  let timer;
  /** One load a moment after a burst of changes. */
  const later = (ms) => {
    clearTimeout(timer);
    timer = setTimeout(load, ms);
  };

  $effect(() => {
    // A new dry run a moment after the limits change.
    const key = limitsOf(draft);
    if (key == null || !valid || key === shownKey) return;
    pending = true;
    later(300);
  });

  onMount(() => {
    load();
    const stops = [];
    for (const name of ['storage-changed', 'matches-changed']) {
      listen(name, () => later(150)).then((stop) => stops.push(stop));
    }
    return () => {
      clearTimeout(timer);
      stops.forEach((stop) => stop());
    };
  });

  /** Saves the limits on screen, with the cleanup on or off. */
  async function save(cleanup) {
    if (!valid || busy) return;
    busy = true;
    message = '';
    try {
      await invoke('set_cleanup', { settings: { ...$state.snapshot(draft), cleanup } });
      draft = { ...draft, cleanup };
      await load();
      draft = { ...overview.settings };
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
      confirming = false;
    }
  }

  /** Turning it on removes what the dry run lists: confirm first, unless that's nothing. */
  function turnOn() {
    if (plan?.removals.length) confirming = true;
    else save(true);
  }

  async function runNow() {
    busy = true;
    message = '';
    try {
      const report = await invoke('run_cleanup');
      const sent = plural(report.removed.length, 'video', 'videos');
      message = report.removed.length
        ? `Sent ${sent} (${size(report.freedBytes)}) to the Recycle Bin.`
        : 'Nothing needed cleaning up.';
      if (report.stopped) message += ' It stopped because a game started.';
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function act(command, id) {
    error = '';
    try {
      if (command === 'keep') await invoke('set_match_favorite', { id, favorite: true });
      else if (command === 'reveal') await invoke('reveal_video', { id });
      else if (command === 'retry') await invoke('retry_cleanup', { id });
      if (command !== 'reveal') await load();
    } catch (e) {
      error = String(e);
    }
  }

  /** What importing the folder would add: reads only. */
  async function look() {
    surveying = true;
    survey = null;
    imported = '';
    error = '';
    try {
      survey = await invoke('outplayed_survey', { root: folder });
    } catch (e) {
      error = String(e);
    } finally {
      surveying = false;
    }
  }

  async function importNow() {
    importing = true;
    error = '';
    try {
      const done = await invoke('import_outplayed', { root: folder });
      imported = `Imported ${plural(done.new, 'video', 'videos')}. They're in the match list.`;
      survey = null;
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      importing = false;
    }
  }

  /** Lets the cleanup remove an import's videos (asking first), or stops it. */
  async function importCleanup(item, allowed) {
    if (allowed && allowing !== item.id) {
      allowing = item.id;
      return;
    }
    allowing = null;
    error = '';
    try {
      await invoke('set_import_cleanup', { id: item.id, allowed });
      await load();
    } catch (e) {
      error = String(e);
    }
  }

  const reason = (removal) =>
    removal.reason === 'age' ? `older than ${saved?.keepDays ?? '?'} days` : 'over the size limit';
</script>

<nav>
  <button type="button" class="back" onclick={onBack}>← All matches</button>
</nav>

{#if error}<p class="error">{error}</p>{/if}

{#if !overview || !draft}
  <p class="muted">Looking at the library…</p>
{:else}
  <section class="card">
    <h2>Storage</h2>
    <div class="usage">
      <div>
        <b>{size(plan.managedBytes)}</b>
        <span>{plural(plan.managedMatches, 'full match', 'full matches')}{plan.favorites ? `, ${plan.favorites} ★` : ''}</span>
      </div>
      {#if plan.unmanagedMatches}
        <div>
          <b>{size(plan.unmanagedBytes)}</b>
          <span>{plural(plan.unmanagedMatches, 'imported match', 'imported matches')}, left alone</span>
        </div>
      {/if}
      <div><b>{size(plan.clipBytes)}</b><span>clips</span></div>
      {#if overview.freeBytes != null}
        <div><b>{size(overview.freeBytes)}</b><span>free on the library's drive</span></div>
      {/if}
    </div>
    {#if plan.clipsOverWarning}
      <p class="warn">
        Clips take more than {saved.clipsWarnGb} GB. They are never removed for you: recycle the ones you don't
        need from their match.
      </p>
    {/if}
  </section>

  <section class="card">
    <h2>
      Cleanup <span class="state" class:on={saved.cleanup}>{saved.cleanup ? 'on' : 'off'}</span>
    </h2>
    <p class="muted">
      Sends full-match videos to the Recycle Bin after each match and every hour, never during a game. Clips are
      never removed, nor are favorites (★ in a match's review).
    </p>
    <div class="limits">
      <label>
        Keep full matches for
        <input type="number" min="0" step="1" bind:value={draft.keepDays} />
        days
      </label>
      <label>
        and at most
        <input type="number" min="0" step="10" bind:value={draft.fullMatchesGb} />
        GB of them
      </label>
      <label>
        Warn when clips pass
        <input type="number" min="0" step="10" bind:value={draft.clipsWarnGb} />
        GB
      </label>
      <span class="muted small">0: no limit.</span>
    </div>
    {#if !valid}<p class="error small">Days are whole numbers; none of these can be negative.</p>{/if}
    {#if !overview.settingsWritable}
      <p class="error small">The settings file couldn't be read when Downplayed started, so it can't be changed here.</p>
    {/if}
    <div class="actions">
      {#if saved.cleanup}
        <button type="button" class="save" disabled={busy || overview.gameRunning} onclick={runNow}>
          {busy ? 'Cleaning up…' : 'Clean up now'}
        </button>
        {#if edited}<button type="button" class="back" disabled={busy || !valid} onclick={() => save(true)}>Save</button>{/if}
        <button type="button" class="back" disabled={busy || !valid} onclick={() => save(false)}>Turn off</button>
      {:else}
        <button type="button" class="save" disabled={busy || pending || !valid || confirming} onclick={turnOn}>
          Turn on…
        </button>
        {#if edited}<button type="button" class="back" disabled={busy || !valid} onclick={() => save(false)}>Save</button>{/if}
      {/if}
      {#if edited}
        <button type="button" class="link" onclick={() => (draft = { ...saved })}>undo changes</button>
      {/if}
      {#if overview.gameRunning}<span class="muted small">not while League is running</span>{/if}
    </div>
    {#if confirming}
      <div class="confirm" role="alertdialog" aria-label="Turn the cleanup on">
        <span>
          This sends the {plural(plan.removals.length, 'video', 'videos')} below ({size(plan.freedBytes)}) to the
          Recycle Bin now, and keeps to these limits after each match.
        </span>
        <button type="button" class="save" disabled={busy} onclick={() => save(true)}>Turn on</button>
        <button type="button" class="back" onclick={() => (confirming = false)}>Cancel</button>
      </div>
    {/if}
    {#if message}<p class="done small">{message}</p>{/if}

    <h3>Dry run{edited ? ', with the limits above (not saved)' : ''}</h3>
    {#if pending}
      <p class="muted small">Working it out…</p>
    {:else if plan.removals.length}
      <p>
        If it ran now, it would send <b>{plural(plan.removals.length, 'full-match video', 'full-match videos')}</b>
        to the Recycle Bin and free <b>{size(plan.freedBytes)}</b>; full matches would then take
        {size(plan.bytesAfter)}.
      </p>
      <ul class="rows">
        {#each plan.removals as removal (removal.id)}
          <li class="removal">
            <button type="button" class="link" onclick={() => onOpen(removal.id)}>{when(removal.whenMs)}</button>
            <span>{removal.champion ?? 'Unknown champion'}</span>
            <span class="num">{size(removal.bytes)}</span>
            <span class="muted">{reason(removal)}</span>
            <button
              type="button"
              class="icon"
              title="Keep it: make it a favorite"
              aria-label="Keep the match of {when(removal.whenMs)}"
              onclick={() => act('keep', removal.id)}>☆</button
            >
          </li>
        {/each}
      </ul>
    {:else}
      <p class="muted">Nothing to remove: the full matches are within the limits.</p>
    {/if}
    {#if !pending && (overview.thumbnails || overview.folders)}
      <p class="muted small">
        It would also clear
        {[
          overview.thumbnails && plural(overview.thumbnails, 'clip thumbnail without its clip', 'clip thumbnails without their clips'),
          overview.folders && plural(overview.folders, 'empty match folder', 'empty match folders'),
        ]
          .filter(Boolean)
          .join(' and ')}.
      </p>
    {/if}
    {#if overview.lastRun}
      {@const last = overview.lastRun}
      <p class="muted small">
        Last cleanup {when(last.atMs)}: {plural(last.removed, 'video', 'videos')}, {size(last.freedBytes)} freed{last.stopped
          ? '; it stopped when a game started'
          : ''}.
      </p>
    {/if}
  </section>

  <section class="card">
    <h2>Outplayed</h2>
    {#each overview.imports as item (item.id)}
      <div class="import">
        <div>
          <b>{item.root}</b>
          <span class="muted">{plural(item.matches, 'video', 'videos')} imported, last on {when(item.atMs)}</span>
        </div>
        <div class="actions">
          {#if item.cleanup}
            <span class="small">The cleanup may send these videos to the Recycle Bin.</span>
            <button type="button" class="back" onclick={() => importCleanup(item, false)}>Stop that</button>
          {:else}
            <span class="muted small">The cleanup leaves these videos alone.</span>
            <button type="button" class="back" onclick={() => importCleanup(item, true)}>Let it remove them…</button>
          {/if}
        </div>
        {#if allowing === item.id}
          <div class="confirm" role="alertdialog" aria-label="Let the cleanup remove imported videos">
            <span>
              When it is on, the cleanup will send this import's old videos from {item.root} to the Recycle Bin, by the
              same limits as Downplayed's own. Favorites stay.
            </span>
            <button type="button" class="save" onclick={() => importCleanup(item, true)}>Allow</button>
            <button type="button" class="back" onclick={() => (allowing = null)}>Cancel</button>
          </div>
        {/if}
      </div>
    {/each}
    <p class="muted">
      Imports an Outplayed library's League videos where they are: Downplayed lists, plays and clips them, and changes
      nothing in the Outplayed folder.
    </p>
    <div class="actions">
      <input class="path" type="text" bind:value={folder} placeholder="D:\Outplayed" aria-label="Outplayed folder" />
      <button type="button" class="back" disabled={!folder.trim() || surveying || importing} onclick={look}>
        {surveying ? 'Looking…' : 'Look'}
      </button>
    </div>
    {#if survey}
      <p>
        {plural(survey.videos, 'League video', 'League videos')} ({size(survey.bytes)}){survey.already
          ? `, ${survey.already} imported before`
          : ''}: <b>{survey.new} new</b> ({size(survey.newBytes)}).
      </p>
      <p class="muted small">
        {#if survey.export}
          Outplayed's records ({survey.records}) give {survey.joined} of them a champion, K/D/A or markers
          ({plural(survey.markers, 'marker', 'markers')}); the rest have their date and length only.
        {:else}
          No Outplayed records next to them: dates and lengths only.
        {/if}
        Results: {survey.wins} won, {survey.unknownResults} unknown (a loss is never guessed).
      </p>
      {#if survey.new}
        <button type="button" class="save" disabled={importing || overview.gameRunning} onclick={importNow}>
          {importing ? 'Importing…' : `Import ${plural(survey.new, 'video', 'videos')}`}
        </button>
      {/if}
    {/if}
    {#if imported}<p class="done small">{imported}</p>{/if}
  </section>

  {#if overview.kept.length}
    <section class="card kept">
      <h2>Couldn't recycle</h2>
      <p>
        Too big for the Recycle Bin, or on a drive without one, so the cleanup left these and deleted nothing.
        Delete them yourself, keep them, or let it try again (after making the Recycle Bin bigger, say).
      </p>
      <ul class="rows">
        {#each overview.kept as kept (kept.id)}
          <li>
            <button type="button" class="link" onclick={() => onOpen(kept.id)}>{when(kept.whenMs)}</button>
            <span>{kept.champion ?? 'Unknown champion'}</span>
            <span class="num">{size(kept.bytes)}</span>
            <span class="buttons">
              <button type="button" class="back" onclick={() => act('reveal', kept.id)}>Show in Explorer</button>
              <button type="button" class="back" onclick={() => act('keep', kept.id)}>Keep it</button>
              <button type="button" class="back" onclick={() => act('retry', kept.id)}>Try again</button>
            </span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
{/if}

<style>
  nav {
    margin: 4px 0 12px;
  }
  .back {
    border: 1px solid var(--line);
    background: var(--card);
    color: var(--text);
    border-radius: 999px;
    padding: 4px 12px;
    font: inherit;
    cursor: pointer;
  }
  .back:hover {
    background: var(--hover);
  }
  .back:disabled,
  .save:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .save {
    border: 0;
    border-radius: 999px;
    padding: 5px 14px;
    background: var(--accent);
    color: var(--on-strong);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    color: var(--text);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .icon {
    border: 0;
    background: none;
    color: var(--muted);
    border-radius: 4px;
    padding: 2px 4px;
    font: inherit;
    font-size: 15px;
    line-height: 1;
    cursor: pointer;
  }
  .icon:hover,
  .icon:focus-visible {
    background: var(--hover);
    color: var(--warn);
  }
  h3 {
    margin: 16px 0 8px;
    font-size: 14px;
  }
  .usage {
    display: flex;
    flex-wrap: wrap;
    gap: 12px 32px;
  }
  .usage b {
    display: block;
    font-size: 18px;
    font-variant-numeric: tabular-nums;
  }
  .usage span {
    color: var(--muted);
    font-size: 12px;
  }
  .warn {
    color: var(--warn);
    margin: 10px 0 0;
  }
  .state {
    margin-left: 6px;
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--line);
    font-size: 12px;
    font-weight: 600;
  }
  .state.on {
    background: var(--accent);
    color: var(--on-strong);
  }
  .limits {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 18px;
    margin: 10px 0;
  }
  .limits input {
    width: 5.5em;
    font: inherit;
    padding: 2px 6px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--text);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 10px;
  }
  .confirm {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 10px;
    margin-top: 10px;
    padding: 10px 12px;
    border: 1px solid var(--warn);
    border-radius: 8px;
  }
  .confirm span {
    flex: 1 1 260px;
  }
  .done {
    color: var(--accent);
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 340px;
    overflow-y: auto;
    border-top: 1px solid var(--line);
  }
  .rows li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 14px;
    padding: 6px 2px;
    border-bottom: 1px solid var(--line);
  }
  .rows .num {
    margin-left: auto;
    font-variant-numeric: tabular-nums;
  }
  .rows li.removal {
    display: grid;
    grid-template-columns: minmax(11em, auto) minmax(6em, 1fr) 5.5em minmax(9em, auto) 2em;
  }
  .removal .num {
    text-align: right;
  }
  .rows .link {
    justify-self: start;
    text-align: left;
  }
  .rows .buttons {
    display: flex;
    gap: 6px;
  }
  .kept {
    border-color: var(--warn);
  }
  .import {
    padding: 8px 0;
    border-bottom: 1px solid var(--line);
    margin-bottom: 8px;
  }
  .path {
    flex: 1 1 280px;
    font: inherit;
    padding: 4px 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--text);
  }
  .small {
    font-size: 12px;
  }
  .error {
    color: var(--lose);
  }
</style>
