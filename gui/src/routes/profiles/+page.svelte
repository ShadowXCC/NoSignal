<script lang="ts">
  // Profiles: saved layouts, hot-switchable; the active one is re-asserted
  // by the daemon on hotplug/reboot.
  import { profiles, refresh } from "$lib/stores";
  import { api } from "$lib/api";

  let newName = $state("");
  let notice = $state<string | null>(null);

  async function run(action: () => Promise<unknown>) {
    notice = null;
    try {
      await action();
      await refresh();
    } catch (e) {
      notice = String(e);
    }
  }

  const saveCurrent = () => {
    const name = newName.trim();
    if (!name) return;
    void run(async () => {
      await api.profileSave(name);
      newName = "";
    });
  };
</script>

<h1>Profiles</h1>

{#if notice}
  <div class="notice">{notice}</div>
{/if}

{#if $profiles?.suspended}
  <div class="notice">
    The active profile was <strong>suspended by the loop guard</strong> (the
    compositor kept fighting re-asserts). Apply it again to resume enforcement.
  </div>
{/if}

<div class="save-row">
  <input
    type="text"
    placeholder="Save current layout as…"
    bind:value={newName}
    onkeydown={(e) => e.key === "Enter" && saveCurrent()}
  />
  <button class="primary" onclick={saveCurrent}>Save profile</button>
</div>

{#if ($profiles?.profiles.length ?? 0) === 0}
  <p class="muted">
    No profiles yet. Arrange your displays the way you like (e.g. TV off),
    then save the layout here. Suggested starters: <em>desk</em> (TV off) and
    <em>movie</em> (TV on).
  </p>
{:else}
  <ul class="list">
    {#each $profiles?.profiles ?? [] as p (p.name)}
      <li>
        <div class="name">
          {p.name}
          {#if p.active}<span class="badge active">active</span>{/if}
          {#if p.drifted}<span class="badge drift">drifted</span>{/if}
          {#if p.hotkey}<span class="badge">{p.hotkey}</span>{/if}
        </div>
        <div class="row-actions">
          <button
            class="primary"
            onclick={() => run(() => api.profileApply(p.name))}
          >
            Apply
          </button>
          <button onclick={() => run(() => api.profileSave(p.name))}>
            Overwrite with current
          </button>
          <button
            onclick={() =>
              confirm(`Delete profile '${p.name}'?`) &&
              run(() => api.profileDelete(p.name))}
          >
            Delete
          </button>
        </div>
      </li>
    {/each}
  </ul>
  <p class="muted">
    Hotkeys are assigned in the profile file for now (<code>hotkey =
    "&lt;Super&gt;F9"</code> in <code>profiles.toml</code>); the daemon binds
    them via the desktop portal on Linux.
  </p>
{/if}

<style>
  .notice {
    padding: 0.6rem 0.9rem;
    border-radius: 8px;
    margin-bottom: 1rem;
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    background: var(--surface);
  }
  .save-row {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1.25rem;
  }
  .save-row input {
    width: 280px;
  }
  .list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 0.7rem 1rem;
  }
  .name {
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .badge {
    font-size: 0.7rem;
    font-weight: 500;
    padding: 0.1rem 0.45rem;
    border-radius: 999px;
    border: 1px solid var(--border);
    color: var(--muted);
  }
  .badge.active {
    border-color: var(--accent);
    color: var(--accent);
  }
  .badge.drift {
    border-color: var(--danger);
    color: var(--danger);
  }
  .row-actions {
    display: flex;
    gap: 0.4rem;
  }
  .muted {
    color: var(--muted);
  }
</style>
