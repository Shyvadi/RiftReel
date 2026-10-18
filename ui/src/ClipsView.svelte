<script>
  import { onMount, tick, untrack } from 'svelte';
  import { convertFileSrc, invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { category, when, words } from './lib/format.js';
  import Icon from './lib/Icon.svelte';

  /** `active`: false while a match's review takes the page's place. */
  let { active = true, onOpen } = $props();

  /** Clips per query. */
  const PAGE = 48;
  /** The most one query returns. */
  const MOST = 500;

  const noFilters = () => ({ champion: '', favorites: false, sort: 'newest' });
  let filters = $state(noFilters());
  let champions = $state([]);
  /** How many clips pass the filters; null until the first page arrives. */
  let total = $state(null);
  /** The clips loaded so far, in order. */
  let clips = $state([]);
  let loading = $state(false);
  let error = $state('');
  /** Answers to queries asked before the last change are dropped. */
  let generation = 0;
  /** The "Show more" button: once it is in view, the next clips load. */
  let sentinel = $state(null);

  /** The clip in the player (which then takes the page's place), or null. */
  let playing = $state(null);
  /** The last clip played, marked in the grid. */
  let lastPlayed = $state(null);
  let videoError = $state('');
  /** The grid's part of the page, the element the page scrolls in, and where it was. */
  let browse = $state(null);
  let scroller = null;
  let gridScroll = 0;
  /** A clip waiting for its second "Recycle" click, and the last action's message. */
  let confirming = $state(null);
  let actionError = $state('');
  let actionNotice = $state('');

  const filtered = $derived(!!(filters.champion || filters.favorites));
  const more = $derived(total !== null && clips.length < total);
  /** A clip's identity: its match and its file. */
  const key = (c) => `${c.matchId}/${c.file}`;
  /** Where the clip in the player is in the grid (-1: not there any more). */
  const at = $derived(playing ? clips.findIndex((c) => key(c) === key(playing)) : -1);

  function query(offset, limit) {
    return {
      offset,
      limit,
      champion: filters.champion || null,
      favorites: filters.favorites,
      oldest: filters.sort === 'oldest',
    };
  }

  /** The clips from the first again. `keep`: as many as are loaded, so the grid keeps its place. */
  async function reload(keep) {
    const asOf = ++generation;
    const limit = keep ? Math.min(MOST, Math.max(PAGE, clips.length)) : PAGE;
    loading = true;
    try {
      const page = await invoke('list_clips', { query: query(0, limit) });
      if (asOf !== generation) return;
      error = '';
      total = page.total;
      clips = page.items;
    } catch (e) {
      if (asOf === generation) error = String(e);
    } finally {
      if (asOf === generation) loading = false;
    }
  }

  /** The clips after the loaded ones. */
  async function loadMore() {
    if (loading || !more) return;
    const asOf = generation;
    loading = true;
    try {
      const page = await invoke('list_clips', { query: query(clips.length, PAGE) });
      if (asOf !== generation) return;
      error = '';
      total = page.total;
      // A clip saved meanwhile moves the rest along: skip the ones already shown.
      const shown = new Set(clips.map(key));
      clips.push(...page.items.filter((c) => !shown.has(key(c))));
    } catch (e) {
      if (asOf === generation) error = String(e);
    } finally {
      if (asOf === generation) loading = false;
    }
  }

  function loadChampions() {
    invoke('match_filters')
      .then((facets) => (champions = facets.champions))
      .catch(() => {});
  }

  // Any change of a filter or the sort starts the grid over. (Not from the inputs'
  // change handlers: those can run before the bound value has changed.)
  let filtersSeen = false;
  $effect(() => {
    JSON.stringify(filters);
    if (!filtersSeen) {
      filtersSeen = true;
      return;
    }
    untrack(() => reload(false));
  });

  // More clips once the end of the grid is near, and again after each load while it is.
  // (Not after a failed load: "Show more" tries again.)
  $effect(() => {
    if (!sentinel || loading || !more || error) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) loadMore();
      },
      { rootMargin: '600px 0px' },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  });

  /** The element the page scrolls in (the window's content column). */
  function scrollParent(node) {
    for (let el = node?.parentElement; el; el = el.parentElement) {
      if (/auto|scroll/.test(getComputedStyle(el).overflowY)) return el;
    }
    return document.scrollingElement;
  }

  onMount(() => {
    loadChampions();
    reload(false);
    scroller = scrollParent(browse);
    const remember = () => {
      if (active && !playing) gridScroll = scroller.scrollTop;
    };
    scroller?.addEventListener('scroll', remember, { passive: true });
    const stops = [];
    listen('matches-changed', () => {
      loadChampions();
      reload(true);
    }).then((stop) => stops.push(stop));
    return () => {
      scroller?.removeEventListener('scroll', remember);
      stops.forEach((stop) => stop());
    };
  });

  // A review opening over the page stops the clip (hidden, it would play on).
  $effect(() => {
    if (!active) playing = null;
  });

  // The player starts at the top of the page; the grid comes back where it was, with the
  // last clip played in view.
  let view = 'grid';
  $effect(() => {
    const now = !active ? 'hidden' : playing ? 'player' : 'grid';
    if (now === view) return;
    view = now;
    tick().then(() => {
      if (!scroller || view !== now) return;
      if (now === 'player') {
        scroller.scrollTop = 0;
      } else if (now === 'grid') {
        scroller.scrollTop = gridScroll;
        browse?.querySelector('li.current')?.scrollIntoView({ block: 'nearest' });
      }
    });
  });

  function play(c) {
    videoError = '';
    confirming = null;
    lastPlayed = key(c);
    playing = c;
  }

  /** The clip before (-1) or after (1) the one in the player. */
  async function step(by) {
    if (at < 0) return;
    if (at + by >= clips.length) await loadMore();
    const next = clips[at + by];
    if (next) play(next);
  }

  /** In the player: Esc goes back to the grid, the arrows to the clip before or after. */
  function keys(event) {
    if (!playing || !active || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    const target = event.target;
    if (target instanceof HTMLVideoElement || target?.closest?.('input, select, textarea')) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      playing = null;
    } else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      event.preventDefault();
      step(event.key === 'ArrowLeft' ? -1 : 1);
    }
  }

  function openMatch(c) {
    playing = null;
    onOpen(c.matchId);
  }

  function clear() {
    filters = noFilters();
  }

  /** Applies `change` to a clip, in the grid and in the player. */
  function update(c, change) {
    const k = key(c);
    for (const item of clips) if (key(item) === k) Object.assign(item, change);
    if (playing && key(playing) === k) Object.assign(playing, change);
  }

  async function toggleFavorite(c) {
    const favorite = !c.favorite;
    actionError = '';
    try {
      await invoke('set_clip_favorite', { id: c.matchId, file: c.file, favorite });
      update(c, { favorite });
    } catch (e) {
      actionError = String(e);
    }
  }

  async function reveal(c) {
    actionError = '';
    try {
      await invoke('reveal_clip', { id: c.matchId, file: c.file });
    } catch (e) {
      actionError = String(e);
    }
  }

  let noticeTimer;
  /** A line over the clips for a few seconds. */
  function notice(text) {
    actionNotice = text;
    clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => (actionNotice = ''), 5000);
  }

  async function copyClip(c) {
    actionError = '';
    try {
      await invoke('copy_clip', { id: c.matchId, file: c.file });
      notice('Copied: paste it in Explorer, Discord or a chat.');
    } catch (e) {
      actionError = String(e);
    }
  }

  async function exportClip(c) {
    actionError = '';
    try {
      const to = await invoke('export_clip', { id: c.matchId, file: c.file });
      if (to) notice(`Saved a copy: ${to}`);
    } catch (e) {
      actionError = String(e);
    }
  }

  /** Sends a clip to the Recycle Bin, on a second click. */
  async function recycleClip(c) {
    const k = key(c);
    if (confirming !== k) {
      confirming = k;
      setTimeout(() => {
        if (confirming === k) confirming = null;
      }, 4000);
      return;
    }
    confirming = null;
    actionError = '';
    try {
      if (!(await invoke('delete_clip', { id: c.matchId, file: c.file }))) return;
      if (playing && key(playing) === k) playing = null;
      clips = clips.filter((other) => key(other) !== k);
      if (total) total -= 1;
      notice('Moved to the Recycle Bin.');
    } catch (e) {
      actionError = String(e);
    }
  }

  const initial = (name) => (name ? name.slice(0, 1).toUpperCase() : '?');
  /** What a clip's buttons call it. */
  const title = (c) => [words(c.label), c.champion].filter(Boolean).join(', ');
  const duration = (c) => `${Math.round(c.toS - c.fromS)} s`;
  const game = (c) => [c.champion, c.queue ?? c.gameMode].filter(Boolean).join(' · ');
</script>

{#snippet actions(c)}
  <button
    type="button"
    class="icon-btn"
    class:on={c.favorite}
    aria-pressed={c.favorite}
    aria-label="Favorite: {title(c)}"
    title={c.favorite ? 'Favorite' : 'Mark as favorite'}
    onclick={() => toggleFavorite(c)}><Icon name={c.favorite ? 'star-filled' : 'star'} /></button
  >
  <button
    type="button"
    class="icon-btn"
    disabled={!c.path}
    title="Show in Explorer"
    aria-label="Show in Explorer: {title(c)}"
    onclick={() => reveal(c)}><Icon name="folder" /></button
  >
  <button
    type="button"
    class="icon-btn"
    disabled={!c.path}
    title="Copy (paste it in Explorer, Discord or a chat)"
    aria-label="Copy: {title(c)}"
    onclick={() => copyClip(c)}><Icon name="copy" /></button
  >
  <button
    type="button"
    class="icon-btn"
    disabled={!c.path}
    title="Save a copy as…"
    aria-label="Save a copy: {title(c)}"
    onclick={() => exportClip(c)}><Icon name="download" /></button
  >
  {#if confirming === key(c)}
    <button
      type="button"
      class="btn small danger recycle"
      aria-label="Confirm: recycle {title(c)}"
      onclick={() => recycleClip(c)}>Recycle?</button
    >
  {:else}
    <button
      type="button"
      class="icon-btn recycle"
      title="Move to the Recycle Bin"
      aria-label="Recycle: {title(c)}"
      onclick={() => recycleClip(c)}><Icon name="trash" /></button
    >
  {/if}
{/snippet}

<svelte:window onkeydown={keys} />

{#if playing}
  {@const c = playing}
  <div class="topbar">
    <button type="button" class="btn ghost" onclick={() => (playing = null)}><Icon name="back" /> All clips</button>
  </div>
  <section class="card player" aria-label="Clip player">
    <div class="player-head">
      <span class="name {category(c.label)}"><i></i><span class="label">{words(c.label)}</span></span>
      <span class="muted">{[game(c), when(c.whenMs)].filter(Boolean).join(' · ')}</span>
      {#if c.result === 'win' || c.result === 'lose'}
        <span class="result {c.result}">{c.result === 'win' ? 'Win' : 'Loss'}</span>
      {/if}
    </div>
    <div class="screen">
      {#if c.path && !videoError}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video
          src={convertFileSrc(c.path)}
          controls
          autoplay
          onerror={() => (videoError = 'This clip could not be played.')}
        ></video>
      {:else}
        <div class="nofile muted">{videoError || "This clip's file is missing."}</div>
      {/if}
    </div>
    <div class="player-actions">
      <button
        type="button"
        class="icon-btn"
        disabled={at <= 0}
        title="Previous clip (←)"
        aria-label="Previous clip"
        onclick={() => step(-1)}><Icon name="back" /></button
      >
      <span class="position muted">{at >= 0 ? `${at + 1} of ${total}` : ''}</span>
      <button
        type="button"
        class="icon-btn"
        disabled={at < 0 || (at + 1 >= clips.length && !more)}
        title="Next clip (→)"
        aria-label="Next clip"
        onclick={() => step(1)}><Icon name="next" /></button
      >
      <span class="divider" aria-hidden="true"></span>
      {@render actions(c)}
      <span class="message" role="status">
        {#if actionError}<span class="error">{actionError}</span>
        {:else if actionNotice}<span class="notice">{actionNotice}</span>{/if}
      </span>
      <button type="button" class="btn small" onclick={() => openMatch(c)}
        ><Icon name="matches" size={18} /> Open match</button
      >
    </div>
  </section>
{/if}

<!-- Kept while a clip plays, so the grid keeps what it loaded. -->
<div bind:this={browse} hidden={!!playing}>
  <header class="page-head">
    <h1>Clips</h1>
    {#if total !== null}<span class="count">{total}{filtered ? ' shown' : ''}</span>{/if}
    <select class="sort" bind:value={filters.sort} aria-label="Sort">
      <option value="newest">Newest first</option>
      <option value="oldest">Oldest first</option>
    </select>
  </header>

  <section class="card list">
    <div class="filters" role="group" aria-label="Filter the clips">
      <select bind:value={filters.champion} aria-label="Champion">
        <option value="">All champions</option>
        {#each champions as champion (champion)}<option value={champion}>{champion}</option>{/each}
      </select>
      <button
        type="button"
        class="chip"
        aria-pressed={filters.favorites}
        onclick={() => (filters.favorites = !filters.favorites)}
        ><Icon name={filters.favorites ? 'star-filled' : 'star'} size={17} /> Favorites</button
      >
      <span class="grow"></span>
      {#if filtered}<button type="button" class="btn ghost small" onclick={clear}>Clear filters</button>{/if}
    </div>

    {#if !playing}
      {#if actionError}<p class="error small">{actionError}</p>{/if}
      {#if actionNotice}<p class="notice">{actionNotice}</p>{/if}
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
    {#if total === null}
      <p class="empty muted">Loading…</p>
    {:else if total === 0}
      <div class="empty">
        {#if filtered}
          <p>No clips fit these filters.</p>
          <button type="button" class="btn" onclick={clear}>Clear filters</button>
        {:else}
          <p><b>No clips yet.</b></p>
          <p class="muted">
            Open a match and save one of its moments, or mark in and out on its video: every clip you save shows up
            here.
          </p>
        {/if}
      </div>
    {:else}
      <ul class="clips">
        {#each clips as c (key(c))}
          <li class:current={lastPlayed === key(c)}>
            <button type="button" class="thumb" disabled={!c.path} aria-label="Play {title(c)}" onclick={() => play(c)}>
              {#if c.thumbnailPath}
                <img src={convertFileSrc(c.thumbnailPath)} alt="" loading="lazy" decoding="async" />
              {:else}
                <span class="nothumb" class:missing={!c.path}>{c.path ? initial(c.champion) : 'File missing'}</span>
              {/if}
              {#if c.path}<span class="overlay"><Icon name="play" size={30} /></span>{/if}
              <span class="length">{duration(c)}</span>
            </button>
            <div class="clip-info">
              <span class="name {category(c.label)}"><i></i><span class="label">{words(c.label)}</span></span>
              {#if c.result === 'win' || c.result === 'lose'}
                <span class="result {c.result}">{c.result === 'win' ? 'Win' : 'Loss'}</span>
              {/if}
            </div>
            <div class="sub">{game(c)}</div>
            <div class="sub">{when(c.whenMs)}</div>
            <div class="clip-actions">{@render actions(c)}</div>
          </li>
        {/each}
      </ul>
      {#if more}
        <div class="more">
          <button type="button" class="btn" bind:this={sentinel} disabled={loading} onclick={loadMore}
            >{loading ? 'Loading…' : 'Show more'}</button
          >
        </div>
      {/if}
    {/if}
  </section>
</div>

<style>
  .sort {
    margin-left: auto;
    align-self: center;
  }
  .list {
    padding: 18px 18px 22px;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    padding: 0 6px 16px;
    margin-bottom: 18px;
    border-bottom: 1px solid var(--line);
  }
  .filters select {
    min-width: 180px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 42px;
    padding: 0 16px;
    border-radius: 999px;
    border: 1px solid var(--line);
    background: var(--raised);
    color: var(--muted);
    font: inherit;
    font-size: 15px;
    font-weight: 600;
    cursor: pointer;
  }
  .chip:hover {
    color: var(--text);
    border-color: var(--muted);
  }
  .chip[aria-pressed='true'] {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--text);
  }
  .chip[aria-pressed='true'] :global(.icon) {
    color: var(--warn);
  }
  .grow {
    flex: 1;
  }
  .empty {
    padding: 48px 12px;
    text-align: center;
    font-size: 16px;
  }
  .empty p {
    margin: 0 0 10px;
  }
  .error.small {
    margin: 0 6px 14px;
    font-size: 14px;
  }
  .notice {
    margin: 0 6px 14px;
    color: var(--accent);
  }

  /* The grid. */
  .clips {
    list-style: none;
    margin: 0;
    padding: 0 6px;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 18px;
  }
  .clips li {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--line);
    border-radius: 12px;
    overflow: hidden;
    background: var(--raised);
  }
  .clips li.current {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .thumb {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    aspect-ratio: 16 / 9;
    padding: 0;
    border: 0;
    background: #000;
    color: #fff;
    cursor: pointer;
  }
  .thumb:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .thumb img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .nothumb {
    font-size: 34px;
    font-weight: 700;
    color: var(--muted);
  }
  .nothumb.missing {
    font-size: 14px;
    font-weight: 400;
    color: #fff;
  }
  .overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.25);
    opacity: 0;
    transition: opacity 120ms;
  }
  .thumb:hover .overlay,
  .thumb:focus-visible .overlay {
    opacity: 1;
  }
  .length {
    position: absolute;
    right: 8px;
    bottom: 8px;
    padding: 2px 8px;
    border-radius: 6px;
    background: rgba(0, 0, 0, 0.72);
    color: #fff;
    font-size: 13px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .clip-info {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 14px 2px;
    font-size: 16px;
  }
  .clip-info .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .result {
    flex: none;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.6px;
    text-transform: uppercase;
  }
  .result.win {
    color: var(--win);
  }
  .result.lose {
    color: var(--lose);
  }
  .sub {
    padding: 0 14px;
    color: var(--muted);
    font-size: 14px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .clip-actions {
    display: flex;
    align-items: center;
    gap: 2px;
    margin-top: auto;
    padding: 8px 8px 10px;
  }
  .clip-actions .recycle {
    margin-left: auto;
  }
  .more {
    display: flex;
    justify-content: center;
    padding: 22px 0 4px;
  }

  /* A moment's colour: the dot before its name. */
  .name.fight {
    color: var(--fight);
  }
  .name.objective {
    color: var(--objective);
  }
  .name.assist {
    color: var(--assist);
  }
  .name.death {
    color: var(--death);
  }
  .name.win {
    color: var(--win);
  }
  .name.custom {
    color: var(--accent);
  }
  i {
    display: inline-block;
    width: 12px;
    height: 12px;
    border-radius: 4px;
    margin-right: 8px;
    background: currentColor;
    vertical-align: -1px;
  }
  .label {
    color: var(--text);
    text-transform: capitalize;
    font-weight: 600;
  }

  /* The player, in the page's place: the video as large as the window lets it be. */
  .topbar {
    margin: -6px 0 14px -10px;
  }
  .player {
    padding: 16px 18px 12px;
  }
  .player-head {
    display: flex;
    align-items: baseline;
    gap: 14px;
    min-width: 0;
    margin: 0 4px 14px;
    font-size: 20px;
  }
  .player-head .name {
    flex: none;
  }
  .player-head .muted {
    flex: 1;
    min-width: 0;
    font-size: 15px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .screen {
    display: flex;
    justify-content: center;
    background: #000;
    border-radius: 10px;
    overflow: hidden;
  }
  video,
  .nofile {
    display: block;
    width: 100%;
    /* The card's width, unless the window is too short for that: the controls and the
       buttons under it stay in view. */
    max-width: calc((100vh - 260px) * 16 / 9);
    aspect-ratio: 16 / 9;
    background: #000;
  }
  .nofile {
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .player-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    margin-top: 10px;
  }
  .position {
    min-width: 5.5em;
    text-align: center;
    font-size: 14px;
    font-variant-numeric: tabular-nums;
  }
  .divider {
    width: 1px;
    height: 24px;
    margin: 0 10px;
    background: var(--line);
  }
  .message {
    flex: 1;
    min-width: 0;
    padding: 0 12px;
    font-size: 14px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .message .notice {
    margin: 0;
  }
</style>
