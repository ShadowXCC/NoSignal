// One store per daemon resource, refreshed on daemon events.

import { writable } from "svelte/store";
import { listen } from "@tauri-apps/api/event";
import { api, type ProfilesInfo, type StatusInfo, type Topology } from "./api";

export const topology = writable<Topology | null>(null);
export const profiles = writable<ProfilesInfo | null>(null);
export const status = writable<StatusInfo | null>(null);
export const daemonReachable = writable<boolean>(true);
export const lastError = writable<string | null>(null);

export async function refresh(): Promise<void> {
  try {
    const [topo, profs, stat] = await Promise.all([
      api.outputs(),
      api.profiles(),
      api.status(),
    ]);
    topology.set(topo);
    profiles.set(profs);
    status.set(stat);
    daemonReachable.set(true);
  } catch (e) {
    daemonReachable.set(false);
    lastError.set(String(e));
  }
}

let started = false;

/** Wire event listeners once (layout onMount). */
export function startEventBridge(): void {
  if (started) return;
  started = true;
  void refresh();
  void listen("daemon-event", () => void refresh());
  void listen("daemon-connected", () => void refresh());
  void listen("daemon-disconnected", () => daemonReachable.set(false));
  // Fallback poll: pending countdowns and missed events.
  setInterval(() => void refresh(), 5000);
}
