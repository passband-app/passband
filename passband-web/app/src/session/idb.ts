// The credential at rest, in IndexedDB. Read and written only by the transport.
import type { Credential, CredentialStore } from "../api/transport";

const DB = "passband";
const STORE = "session";
const KEY = "credential";

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function run<T>(mode: IDBTransactionMode, op: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  return open().then(
    (db) =>
      new Promise<T>((resolve, reject) => {
        const tx = db.transaction(STORE, mode);
        const req = op(tx.objectStore(STORE));
        tx.oncomplete = () => {
          db.close();
          resolve(req.result);
        };
        tx.onerror = () => {
          db.close();
          reject(tx.error);
        };
      }),
  );
}

function isCredential(v: unknown): v is Credential {
  return (
    typeof v === "object" &&
    v !== null &&
    typeof (v as Credential).server === "string" &&
    typeof (v as Credential).token === "string"
  );
}

export class IdbCredentialStore implements CredentialStore {
  async load(): Promise<Credential | null> {
    try {
      const v = await run("readonly", (s) => s.get(KEY));
      return isCredential(v) ? v : null;
    } catch {
      return null;
    }
  }
  async save(c: Credential): Promise<void> {
    await run("readwrite", (s) => s.put({ server: c.server, token: c.token }, KEY));
  }
  async clear(): Promise<void> {
    try {
      await run("readwrite", (s) => s.delete(KEY));
    } catch {
      // Nothing stored is the state we wanted.
    }
  }
}
