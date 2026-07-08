<script lang="ts">
  import { onMount } from "svelte";
  import { status } from "$lib/stores";
  import { api } from "$lib/api";
  import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";

  let closeToTray = $state(true);
  let autostart = $state(false);
  let notice = $state<string | null>(null);

  onMount(async () => {
    closeToTray = localStorage.getItem("closeToTray") !== "false";
    await api.setCloseToTray(closeToTray);
    try {
      autostart = await isEnabled();
    } catch {
      // plugin unavailable (dev builds)
    }
  });

  async function toggleCloseToTray() {
    closeToTray = !closeToTray;
    localStorage.setItem("closeToTray", String(closeToTray));
    await api.setCloseToTray(closeToTray);
  }

  async function toggleAutostart() {
    try {
      if (autostart) {
        await disable();
        autostart = false;
      } else {
        await enable();
        autostart = true;
      }
    } catch (e) {
      notice = String(e);
    }
  }
</script>

<h1>Settings</h1>

{#if notice}
  <div class="notice">{notice}</div>
{/if}

<section>
  <h2>Behavior</h2>
  <label class="row">
    <input
      type="checkbox"
      checked={closeToTray}
      onchange={toggleCloseToTray}
    />
    <span>
      <strong>Close to tray</strong>
      <small>Closing the window keeps NoSignal running in the tray.</small>
    </span>
  </label>
  <label class="row">
    <input type="checkbox" checked={autostart} onchange={toggleAutostart} />
    <span>
      <strong>Start GUI on login</strong>
      <small>
        The daemon has its own autostart (systemd user unit on Linux); this
        toggles the tray/window app.
      </small>
    </span>
  </label>
</section>

<section>
  <h2>Daemon</h2>
  {#if $status}
    <dl>
      <dt>Version</dt>
      <dd>{$status.version}</dd>
      <dt>Backend</dt>
      <dd>{$status.backend}</dd>
      <dt>Outputs</dt>
      <dd>{$status.outputs_enabled} of {$status.outputs_total} enabled</dd>
      <dt>Active profile</dt>
      <dd>
        {$status.active_profile ?? "none"}
        {#if $status.drifted}(drifted){/if}
        {#if $status.suspended}(suspended by loop guard){/if}
      </dd>
    </dl>
  {:else}
    <p class="muted">Daemon not reachable.</p>
    <button class="primary" onclick={() => api.daemonStart()}>
      Start daemon
    </button>
  {/if}
  <p class="muted">
    The revert-timer default (20 s) and output aliases live in the daemon
    config (<code>~/.config/nosignal/config.toml</code> on Linux). Automation:
    the daemon's DBus API <code>io.github.shadowxcc.NoSignal.Daemon1</code> or
    <code>nosignal --json</code>.
  </p>
</section>

<style>
  section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 1rem 1.25rem;
    margin-bottom: 1rem;
    max-width: 640px;
  }
  h2 {
    font-size: 1rem;
    margin-top: 0;
  }
  .row {
    display: flex;
    gap: 0.7rem;
    align-items: flex-start;
    padding: 0.5rem 0;
    cursor: pointer;
  }
  .row span {
    display: flex;
    flex-direction: column;
  }
  .row small {
    color: var(--muted);
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.3rem 0.9rem;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
  .muted {
    color: var(--muted);
  }
  .notice {
    padding: 0.6rem 0.9rem;
    border-radius: 8px;
    margin-bottom: 1rem;
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    background: var(--surface);
  }
</style>
