import { toasts, useToasts } from "../app/toasts";
import { useKeys } from "./keys";

export function Toasts() {
  const list = useToasts();
  useKeys({ u: () => void toasts.undoLatest() });
  return (
    <div className="toasts" aria-live="polite">
      {list.map((t) => (
        <div key={t.id} className={`toast ${t.tone}`}>
          <span>{t.text}</span>
          {t.undo && (
            <button
              className="link"
              onClick={() => {
                toasts.dismiss(t.id);
                void t.undo?.();
              }}
            >
              undo <kbd>u</kbd>
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
