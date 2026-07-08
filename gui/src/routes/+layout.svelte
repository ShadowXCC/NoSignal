<script lang="ts">
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import { startEventBridge, daemonReachable, status } from "$lib/stores";
  import { api } from "$lib/api";
  import PendingBanner from "$lib/PendingBanner.svelte";

  let { children } = $props();

  onMount(() => startEventBridge());

  async function startDaemon() {
    try {
      await api.daemonStart();
    } catch (e) {
      console.error(e);
    }
  }

  const nav = [
    { href: "/", label: "Displays" },
    { href: "/profiles", label: "Profiles" },
    { href: "/settings", label: "Settings" },
  ];
</script>

<div class="shell">
  <nav>
    <div class="brand">
      <span class="dot"></span> NoSignal
    </div>
    {#each nav as item (item.href)}
      <a href={item.href} class:active={page.url.pathname === item.href}>
        {item.label}
      </a>
    {/each}
    <div class="spacer"></div>
    {#if $status}
      <div class="backend">backend: {$status.backend}</div>
    {/if}
  </nav>

  <main>
    {#if !$daemonReachable}
      <div class="banner warn">
        The NoSignal daemon isn't reachable — persistence and hotkeys are
        offline.
        <button onclick={startDaemon}>Start daemon</button>
      </div>
    {/if}
    <PendingBanner />
    {@render children()}
  </main>
</div>

<style>
  :global(:root) {
    --bg: #f5f6f8;
    --surface: #ffffff;
    --text: #1a1d21;
    --muted: #6a7178;
    --accent: #3b82f6;
    --danger: #dc2626;
    --border: #e2e5e9;
    color-scheme: light dark;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg: #131519;
      --surface: #1c1f26;
      --text: #e8eaed;
      --muted: #9aa2ab;
      --accent: #60a5fa;
      --danger: #f87171;
      --border: #2a2e36;
    }
  }
  :global(body) {
    margin: 0;
    font-family:
      system-ui,
      -apple-system,
      "Segoe UI",
      Cantarell,
      sans-serif;
    background: var(--bg);
    color: var(--text);
  }
  :global(button) {
    font: inherit;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
    border-radius: 6px;
    padding: 0.35rem 0.8rem;
    cursor: pointer;
  }
  :global(button:hover) {
    border-color: var(--accent);
  }
  :global(button.primary) {
    background: var(--accent);
    border-color: var(--accent);
    color: white;
  }
  :global(input[type="text"]) {
    font: inherit;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
    border-radius: 6px;
    padding: 0.35rem 0.6rem;
  }

  .shell {
    display: flex;
    height: 100vh;
  }
  nav {
    width: 180px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 1rem 0.75rem;
    border-right: 1px solid var(--border);
    background: var(--surface);
  }
  .brand {
    font-weight: 700;
    margin-bottom: 1rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--accent);
    display: inline-block;
  }
  nav a {
    color: var(--muted);
    text-decoration: none;
    padding: 0.45rem 0.7rem;
    border-radius: 6px;
  }
  nav a.active {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--text);
  }
  nav a:hover {
    color: var(--text);
  }
  .spacer {
    flex: 1;
  }
  .backend {
    font-size: 0.75rem;
    color: var(--muted);
  }
  main {
    flex: 1;
    overflow: auto;
    padding: 1.25rem 1.5rem;
  }
  .banner {
    padding: 0.6rem 0.9rem;
    border-radius: 8px;
    margin-bottom: 1rem;
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .banner.warn {
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
  }
</style>
