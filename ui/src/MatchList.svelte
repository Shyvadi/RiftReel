<script>
  import { onMount, untrack } from 'svelte';
  import { convertFileSrc, invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { clock, when } from './lib/format.js';
  import Icon from './lib/Icon.svelte';

  let { onOpen } = $props();

  /** Every row is this tall, so the list can place any of them without measuring. */
  const ROW = 100;
  /** Matches per query. */
  const PAGE = 100;
  /** Rows drawn beyond the visible ones, each way. */
  const OVERSCAN = 6;

  const noFilters = () => ({
    champion: '',
    queue: '',
    result: '',
    hasClips: false,
    hasMultikill: false,
    from: '',
    to: '',
    sort: 'newest',
  });
  let filters = $state(noFilters());
  /** The date filter as picked: a preset, or 'custom' for the two date fields. */
  let period = $state('any');
  const periods = [
    { value: 'any', label: 'Any time' },
    { value: '7', label: 'Last 7 days' },
    { value: '30', label: 'Last 30 days' },
    { value: '90', label: 'Last 90 days' },
    { value: 'year', label: 'This year' },
    { value: 'custom', label: 'Custom dates…' },
  ];
  let facets = $state({ champions: [], queues: [] });
  /** How many matches pass the filters; null until the first page arrives. */
  let total = $state(null);
  /** Loaded pages by number. */
  let pages = $state({});
  let error = $state('');
  let scrollTop = $state(0);
  let height = $state(0);
  let scroller = $state(null);
  /** Pages being fetched, and the filters they were asked with (late answers are dropped). */
  let fetching = new Set();
  let generation = 0;
  /** Thumbnails already asked for. */
  const asked = new Set();

  const filtered = $derived(
    !!(
      filters.champion ||
      filters.queue ||
      filters.result ||
      filters.hasClips ||
      filters.hasMultikill ||
      filters.from ||
      filters.to
    ),
  );
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN));
  const last = $derived(Math.min(total ?? 0, Math.ceil((scrollTop + height) / ROW) + OVERSCAN));
  const rows = $derived(
    Array.from({ length: Math.max(0, last - first) }, (_, k) => {
      const index = first + k;
      return { index, match: pages[Math.floor(index / PAGE)]?.[index % PAGE] ?? null };
    }),
  );

  /** A date field (local day) as Unix ms; `next` gives the day after's start. */
  function dayMs(value, next = false) {
    if (!value) return null;
    const day = new Date(`${value}T00:00:00`);
    if (next) day.setDate(day.getDate() + 1);
    return day.getTime();
  }

  function query(page) {
    return {
      offset: page * PAGE,
      limit: PAGE,
      champion: filters.champion || null,
      queue: filters.queue || null,
      result: filters.result || null,
      hasClips: filters.hasClips,
      hasMultikill: filters.hasMultikill,
      fromMs: dayMs(filters.from),
      toMs: dayMs(filters.to, true),
      sort: filters.sort,
    };
  }

  async function load(page) {
    if (pages[page] || fetching.has(page)) return;
    fetching.add(page);
    const asOf = generation;
    try {
      const result = await invoke('list_matches', { query: query(page) });
      if (asOf !== generation) return;
      error = '';
      total = result.total;
      pages[page] = result.items;
      // For profiling: when the list first had rows (DevTools' performance timeline).
      if (!performance.getEntriesByName('downplayed:first-rows').length) {
        performance.mark('downplayed:first-rows');
      }
    } catch (e) {
      if (asOf === generation) error = String(e);
    } finally {
      fetching.delete(page);
    }
  }

  function loadFacets() {
    invoke('match_filters')
      .then((value) => (facets = value))
      .catch(() => {});
  }

  /** Drops what was loaded; the visible pages come again (placeholders meanwhile).
   *  `top`: back to the start. */
  function reload(top) {
    generation += 1;
    fetching = new Set();
    pages = {};
    if (top) {
      if (scroller) scroller.scrollTop = 0;
      scrollTop = 0;
    }
    load(0);
  }

  // Any change of a filter or the sort starts the list over. (Not from the inputs'
  // change handlers: those can run before the bound value has changed.)
  let filtersSeen = false;
  $effect(() => {
    JSON.stringify(filters);
    if (!filtersSeen) {
      filtersSeen = true;
      return;
    }
    untrack(() => reload(true));
  });

  // The pages the visible rows are on.
  $effect(() => {
    if (total === null) return;
    const from = Math.floor(first / PAGE);
    const to = Math.floor(Math.max(first, last - 1) / PAGE);
    for (let page = from; page <= to; page++) load(page);
  });

  // Thumbnails for the recorded matches on screen that have none yet.
  $effect(() => {
    const ids = rows
      .map((row) => row.match)
      .filter((match) => match?.video && !match.thumbnailPath && !asked.has(match.id))
      .map((match) => match.id);
    if (!ids.length) return;
    ids.forEach((id) => asked.add(id));
    invoke('request_thumbnails', { ids }).catch(() => {});
  });

  onMount(() => {
    loadFacets();
    load(0);
    const stops = [];
    listen('matches-changed', () => {
      loadFacets();
      reload(false);
    }).then((stop) => stops.push(stop));
    listen('match-thumbnail', (event) => {
      for (const items of Object.values(pages)) {
        const match = items.find((m) => m.id === event.payload.id);
        if (match) match.thumbnailPath = event.payload.thumbnailPath;
      }
    }).then((stop) => stops.push(stop));
    return () => stops.forEach((stop) => stop());
  });

  function clear() {
    filters = noFilters();
    period = 'any';
  }

  /** `day` as the date fields write it (local). */
  const dateText = (day) =>
    `${day.getFullYear()}-${String(day.getMonth() + 1).padStart(2, '0')}-${String(day.getDate()).padStart(2, '0')}`;

  /** A preset sets the date fields; 'custom' leaves them to the user. */
  function pickPeriod(value) {
    period = value;
    if (value === 'custom') return;
    const today = new Date();
    if (value === 'any') {
      filters.from = '';
    } else if (value === 'year') {
      filters.from = `${today.getFullYear()}-01-01`;
    } else {
      const from = new Date(today);
      from.setDate(from.getDate() - (Number(value) - 1));
      filters.from = dateText(from);
    }
    filters.to = '';
  }

  const initial = (name) => (name ? name.slice(0, 1).toUpperCase() : '?');
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
</script>

<header class="page-head">
  <h1>Matches</h1>
  {#if total !== null}<span class="count">{total}{filtered ? ' shown' : ''}</span>{/if}
  <select class="sort" bind:value={filters.sort} aria-label="Sort">
    <option value="newest">Newest first</option>
    <option value="oldest">Oldest first</option>
    <option value="longest">Longest first</option>
    <option value="most_kills">Most kills first</option>
  </select>
</header>

<section class="card list">
  <div class="filters" role="group" aria-label="Filter and sort the matches">
    <select bind:value={filters.champion} aria-label="Champion">
      <option value="">All champions</option>
      {#each facets.champions as champion (champion)}<option value={champion}>{champion}</option>{/each}
    </select>
    <select bind:value={filters.queue} aria-label="Queue">
      <option value="">All queues</option>
      {#each facets.queues as queue (queue)}<option value={queue}>{queue}</option>{/each}
    </select>
    <select bind:value={filters.result} aria-label="Result">
      <option value="">Any result</option>
      <option value="win">Wins</option>
      <option value="lose">Losses</option>
      <option value="unknown">Unknown</option>
    </select>
    <select value={period} onchange={(event) => pickPeriod(event.currentTarget.value)} aria-label="When">
      {#each periods as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
    </select>
    <button
      type="button"
      class="chip"
      aria-pressed={filters.hasClips}
      onclick={() => (filters.hasClips = !filters.hasClips)}>With clips</button
    >
    <button
      type="button"
      class="chip"
      aria-pressed={filters.hasMultikill}
      onclick={() => (filters.hasMultikill = !filters.hasMultikill)}>Multikills</button
    >
    {#if period === 'custom'}
      <span class="dates">
        <input type="date" bind:value={filters.from} aria-label="From" />
        <span class="muted">to</span>
        <input type="date" bind:value={filters.to} aria-label="To" />
      </span>
    {/if}
    <span class="grow"></span>
    {#if filtered}<button type="button" class="btn ghost small" onclick={clear}>Clear filters</button>{/if}
  </div>

  {#if error}<p class="error">{error}</p>{/if}
  {#if total === null}
    <p class="empty muted">Loading…</p>
  {:else if total === 0}
    <div class="empty">
      {#if filtered}
        <p>No matches fit these filters.</p>
        <button type="button" class="btn" onclick={clear}>Clear filters</button>
      {:else}
        <p><b>No matches yet.</b></p>
        <p class="muted">Play a game (Practice Tool works): Downplayed records it, and its review opens here afterwards.</p>
      {/if}
    </div>
  {:else}
    <div
      class="scroller"
      bind:this={scroller}
      bind:clientHeight={height}
      onscroll={(event) => (scrollTop = event.currentTarget.scrollTop)}
    >
      <ul class="matches" style:height="{total * ROW}px">
        {#each rows as row (row.index)}
          {@const match = row.match}
          <li style:transform="translateY({row.index * ROW}px)">
            {#if match}
              <button
                type="button"
                class="row"
                class:win={match.result === 'win'}
                class:lose={match.result === 'lose'}
                onclick={() => onOpen(match.id)}
              >
                <span class="thumb">
                  {#if match.thumbnailPath}
                    <img src={convertFileSrc(match.thumbnailPath)} alt="" loading="lazy" decoding="async" />
                  {:else}
                    <span class="initial">{initial(match.champion)}</span>
                  {/if}
                  {#if match.durationS}<span class="length">{clock(match.durationS)}</span>{/if}
                </span>
                <span class="who">
                  <span class="name">
                    <b>{match.champion ?? 'Unknown champion'}</b>
                    {#if match.result === 'win' || match.result === 'lose'}
                      <span class="result">{match.result === 'win' ? 'Win' : 'Loss'}</span>
                    {/if}
                  </span>
                  <span class="sub">
                    {[match.queue ?? match.gameMode, when(match.whenMs)].filter(Boolean).join(' · ')}
                  </span>
                </span>
                <span class="kda">
                  <b>{match.kills ?? '–'} / {match.deaths ?? '–'} / {match.assists ?? '–'}</b>
                  <span>K / D / A</span>
                </span>
                <span class="tags">
                  {#if match.multikills}<span class="pill hot">{plural(match.multikills, 'multikill', 'multikills')}</span>{/if}
                  {#if match.clips}<span class="pill"
                      >{plural(match.clips, 'clip', 'clips')}{match.favoriteClips ? ' ★' : ''}</span
                    >{/if}
                  {#if match.source === 'recording'}<span class="pill">Recording</span>{/if}
                  {#if match.source === 'outplayed'}<span class="pill">Outplayed</span>{/if}
                  {#if !match.video && match.source !== 'recording'}<span class="pill quiet">No video</span>{/if}
                </span>
                <span class="go" aria-hidden="true"><Icon name="play" size={18} /></span>
              </button>
            {:else}
              <div class="placeholder"></div>
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</section>

<style>
  .list {
    flex: 1;
    min-height: 360px;
    display: flex;
    flex-direction: column;
    padding: 18px 18px 8px;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    padding: 0 6px 16px;
    border-bottom: 1px solid var(--line);
  }
  .filters select {
    min-width: 140px;
  }
  .sort {
    margin-left: auto;
    align-self: center;
  }
  .chip {
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
  .dates {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 15px;
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
  .scroller {
    position: relative;
    flex: 1;
    min-height: 0;
    margin-top: 8px;
    overflow-y: auto;
    contain: strict;
  }
  .matches {
    position: relative;
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 100px;
    padding: 4px 0;
  }
  .row {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    height: 100%;
    display: grid;
    /* Fixed columns, so rows line up (each row is its own grid). */
    grid-template-columns: 148px minmax(0, 1fr) 150px minmax(0, 250px) 28px;
    align-items: center;
    gap: 20px;
    padding: 0 16px 0 10px;
    border-radius: var(--radius-sm);
    border-left: 4px solid transparent;
    cursor: pointer;
  }
  .row:hover,
  .row:focus-visible {
    background: var(--hover);
  }
  .row:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .row.win {
    border-left-color: var(--win);
  }
  .row.lose {
    border-left-color: var(--lose);
  }
  .placeholder {
    height: 100%;
    margin: 0 4px;
    background: linear-gradient(90deg, var(--hover), transparent);
    border-radius: var(--radius-sm);
    opacity: 0.6;
  }
  .thumb {
    position: relative;
    width: 148px;
    height: 84px;
    border-radius: 8px;
    overflow: hidden;
    background: var(--raised);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .initial {
    font-size: 30px;
    font-weight: 700;
    color: var(--muted);
  }
  .length {
    position: absolute;
    right: 6px;
    bottom: 6px;
    padding: 1px 7px;
    border-radius: 6px;
    background: rgba(0, 0, 0, 0.72);
    color: #fff;
    font-size: 13px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .who {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .name {
    display: flex;
    align-items: baseline;
    gap: 12px;
    min-width: 0;
  }
  .name b {
    font-size: 19px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .result {
    flex: none;
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.6px;
    text-transform: uppercase;
    color: var(--muted);
  }
  .row.win .result {
    color: var(--win);
  }
  .row.lose .result {
    color: var(--lose);
  }
  .sub {
    color: var(--muted);
    font-size: 15px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .kda {
    display: grid;
    justify-items: end;
    gap: 2px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .kda b {
    font-size: 19px;
  }
  .kda span {
    color: var(--muted);
    font-size: 12px;
    letter-spacing: 0.6px;
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    justify-content: flex-end;
    max-height: 60px;
    overflow: hidden;
  }
  .pill.hot {
    border-color: var(--fight);
    color: var(--fight);
  }
  .pill.quiet {
    color: var(--muted);
  }
  .go {
    color: var(--muted);
    opacity: 0;
  }
  .row:hover .go,
  .row:focus-visible .go {
    opacity: 1;
    color: var(--accent);
  }
  @media (max-width: 1100px) {
    .row {
      grid-template-columns: 148px minmax(0, 1fr) 130px 28px;
    }
    .tags {
      display: none;
    }
  }
</style>
