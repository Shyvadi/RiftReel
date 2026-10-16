<script>
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import StatusPanel from './StatusPanel.svelte';
  import MatchList from './MatchList.svelte';
  import MatchView from './MatchView.svelte';
  import StorageView from './StorageView.svelte';
  import { clock, words } from './lib/format.js';

  let app = $state(null);
  let error = $state('');
  /** The match under review, or null for the page below. */
  let openId = $state(null);
  /** 'matches' (the overview) or 'storage'. */
  let page = $state('matches');
  const go = (next) => {
    openId = null;
    page = next;
  };
  /** Messages for the user (low disk), shown until dismissed. */
  let notices = $state([]);
  const addNotice = (text) => {
    if (text && !notices.includes(text)) notices.push(text);
  };

  onMount(() => {
    const stops = [];
    invoke('get_status')
      .then((status) => {
        app = status;
        if (status.openMatch) openId = status.openMatch;
        addNotice(status.notice);
      })
      .catch((e) => (error = String(e)));
    listen('status', (event) => {
      if (app) app.tracker = event.payload;
    }).then((stop) => stops.push(stop));
    listen('open-match', (event) => (openId = event.payload)).then((stop) => stops.push(stop));
    listen('notice', (event) => addNotice(event.payload)).then((stop) => stops.push(stop));
    return () => stops.forEach((stop) => stop());
  });
</script>

<main>
  <header>
    <h1>Downplayed</h1>
    {#if app}<span class="muted">v{app.version}</span>{/if}
    <nav class="pages" aria-label="Pages">
      <button type="button" class:active={page === 'matches' && !openId} onclick={() => go('matches')}>Matches</button>
      <button type="button" class:active={page === 'storage' && !openId} onclick={() => go('storage')}>Storage</button>
    </nav>
  </header>

  {#if error}
    <p class="error">{error}</p>
  {/if}
  {#each notices as notice (notice)}
    <div class="notice" role="status">
      <span>{notice}</span>
      <button type="button" aria-label="Dismiss" onclick={() => (notices = notices.filter((n) => n !== notice))}
        >×</button
      >
    </div>
  {/each}

  {#if openId}
    <MatchView id={openId} onBack={() => (openId = null)} />
  {/if}
  {#if app && page === 'storage'}
    <div hidden={!!openId}>
      <StorageView onBack={() => go('matches')} onOpen={(id) => (openId = id)} />
    </div>
  {/if}
  {#if app}
    {@const tracker = app.tracker}
    <!-- Kept while a review is open, so the list keeps its place. -->
    <div hidden={!!openId || page !== 'matches'}>
    <StatusPanel {tracker} />

    {#if tracker.current?.tracking}
      {@const current = tracker.current}
      <section class="card">
        <h2>Current match</h2>
        <p class="facts">
          <span>{current.champion ?? 'champion ?'}</span>
          <span>{current.gameMode ?? 'mode ?'}</span>
          <span>game time {clock(current.gameTimeS)}</span>
          <span>{current.markers} markers</span>
        </p>
        {#if current.moments.length}
          <ol class="moments">
            {#each current.moments as moment, i (i)}
              <li>
                <span class="time">{clock(moment.startS)}–{clock(moment.endS)}</span>
                <span class="label">{words(moment.label)}</span>
              </li>
            {/each}
          </ol>
        {/if}
      </section>
    {/if}

    <MatchList onOpen={(id) => (openId = id)} />

    <footer class="muted">
      <div>Matches <code>{app.matchesDir}</code></div>
      <div>Recordings <code>{app.recordingsDir}</code></div>
      <div>Settings <code>{app.settingsFile}</code> · Logs <code>{app.logDir}</code></div>
      <div>Closing this window leaves Downplayed running in the tray.</div>
    </footer>
    </div>
  {/if}
</main>

<style>
  :global(:root) {
    --bg: #f6f7f9;
    --card: #ffffff;
    --hover: #f0f2f5;
    --text: #1d2230;
    --muted: #6b7385;
    --line: #dde1e8;
    --accent: #0f9e8a;
    --win: #138a52;
    --lose: #b5373a;
    --warn: #b7791f;
    --on-strong: #ffffff;
    --fight: #d4373d;
    --objective: #7c4ddb;
    --assist: #2f6fd6;
    --death: #7a8294;
    color-scheme: light dark;
    font-family: 'Segoe UI Variable', 'Segoe UI', system-ui, sans-serif;
    font-size: 14px;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg: #12151c;
      --card: #1b1f29;
      --hover: #232836;
      --text: #e7eaf0;
      --muted: #8a92a5;
      --line: #2b3140;
      --accent: #3fd1b9;
      --win: #4ccb8a;
      --lose: #f07073;
      --warn: #e8b04e;
      --on-strong: #0d1017;
      --fight: #f2777b;
      --objective: #a98bf5;
      --assist: #6aa3f7;
      --death: #9aa1b2;
    }
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
    color: var(--text);
  }
  :global(.card) {
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 14px 16px;
    margin-bottom: 14px;
  }
  :global(.card h2) {
    margin: 0 0 10px;
    font-size: 15px;
  }
  :global(.muted) {
    color: var(--muted);
  }
  main {
    max-width: 920px;
    margin: 0 auto;
    padding: 20px 24px 32px;
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin-bottom: 14px;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    letter-spacing: 0.2px;
  }
  .pages {
    margin-left: auto;
    display: flex;
    gap: 4px;
  }
  .pages button {
    border: 1px solid transparent;
    background: none;
    color: var(--muted);
    border-radius: 999px;
    padding: 4px 12px;
    font: inherit;
    cursor: pointer;
  }
  .pages button:hover {
    color: var(--text);
  }
  .pages button.active {
    border-color: var(--line);
    background: var(--card);
    color: var(--text);
    font-weight: 600;
  }
  .error {
    color: var(--lose);
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 14px;
    margin-bottom: 14px;
    border: 1px solid var(--warn);
    border-radius: 10px;
    background: var(--card);
  }
  .notice span {
    flex: 1;
  }
  .notice button {
    border: 0;
    background: none;
    color: var(--muted);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    margin: 0 0 10px;
  }
  .moments {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .moments li {
    display: grid;
    grid-template-columns: 120px 1fr;
    gap: 12px;
    padding: 6px 0;
    border-top: 1px solid var(--line);
  }
  .time {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .label {
    text-transform: capitalize;
    font-weight: 600;
  }
  footer {
    margin-top: 24px;
    display: grid;
    gap: 4px;
    font-size: 12px;
  }
  code {
    font-size: 12px;
    word-break: break-all;
  }
</style>
