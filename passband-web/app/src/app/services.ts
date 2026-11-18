import { Client } from "../api/client";
import { InPageTransport, MemoryCredentialStore, type CredentialStore } from "../api/transport";
import { IdbCredentialStore } from "../session/idb";

function credentialStore(): CredentialStore {
  // A browser with no IndexedDB (some private modes) still works for the
  // visit; it just asks to sign in again next time.
  return typeof indexedDB === "undefined" ? new MemoryCredentialStore() : new IdbCredentialStore();
}

export const transport = new InPageTransport(credentialStore());
export const client = new Client(transport);

export const CONTROL_URL =
  import.meta.env.VITE_CONTROL_URL ?? "https://signup.passband.app/app/auth";
export const DESKTOP_DOWNLOAD_URL =
  import.meta.env.VITE_DESKTOP_DOWNLOAD_URL ?? "https://passband.app/download";
