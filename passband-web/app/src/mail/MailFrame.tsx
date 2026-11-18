// PLACEHOLDER until the sandboxed reading frame lands (ported from
// passband-web/spikes/email-render). Renders the plain-text body only, as text,
// so nothing from the mail is ever interpreted as markup.
import type { ClientMessage } from "../api/wire";

export interface ImageSource {
  attachment(attachmentId: number, signal?: AbortSignal): Promise<Blob>;
  remote(url: string, signal?: AbortSignal): Promise<Blob>;
}

export interface MailFrameProps {
  message: ClientMessage;
  images: ImageSource;
  /** The reader opted in to remote images for this message. */
  allowRemoteImages: boolean;
  /** The mail has remote images the reader could choose to load. */
  onHasRemoteImages?: (has: boolean) => void;
  /** A link the reader clicked (http/https only); the app opens it. */
  onLink: (url: string) => void;
}

export function MailFrame({ message }: MailFrameProps) {
  return <pre className="mail-text">{message.content}</pre>;
}
