// Line icons in the Mac app's style (SF Symbols' regular weight, 1.6px strokes
// on a 24px grid). Inline SVG, so there is no icon font or package to load.
import type { ReactNode } from "react";

function Icon({ children, size = 18, label }: { children: ReactNode; size?: number; label?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      {children}
    </svg>
  );
}

type P = { size?: number; label?: string };

export const GaugeIcon = (p: P) => (
  <Icon {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 12l4-3" />
    <path d="M7.5 15.5h.01M12 6.5v.01M16.5 15.5h.01M6.8 9.6h.01" />
  </Icon>
);
export const MailIcon = (p: P) => (
  <Icon {...p}>
    <rect x="3" y="5.5" width="18" height="13" rx="2.5" />
    <path d="M3.5 7l8.5 6 8.5-6" />
  </Icon>
);
export const EyeIcon = (p: P) => (
  <Icon {...p}>
    <path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z" />
    <circle cx="12" cy="12" r="3" />
  </Icon>
);
export const ReadingIcon = (p: P) => (
  <Icon {...p}>
    <path d="M3 9l9-5.5L21 9v9.5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
    <path d="M3.5 9.5l8.5 5.5 8.5-5.5" />
  </Icon>
);
export const CalendarIcon = (p: P) => (
  <Icon {...p}>
    <rect x="3.5" y="5" width="17" height="15" rx="2.5" />
    <path d="M3.5 9.5h17M8 3v4M16 3v4" />
    <path d="M8 13h.01M12 13h.01M16 13h.01M8 16.5h.01M12 16.5h.01" />
  </Icon>
);
export const BoxIcon = (p: P) => (
  <Icon {...p}>
    <path d="M12 2.8l8.5 4.6v9.2L12 21.2l-8.5-4.6V7.4z" />
    <path d="M3.8 7.6L12 12l8.2-4.4M12 12v9" />
    <path d="M7.8 5.1l8.4 4.6" />
  </Icon>
);
export const BankIcon = (p: P) => (
  <Icon {...p}>
    <path d="M3 9.5L12 4l9 5.5" />
    <path d="M5 10v7M9.5 10v7M14.5 10v7M19 10v7" />
    <path d="M3.5 20h17" />
  </Icon>
);
export const ReceiptIcon = (p: P) => (
  <Icon {...p}>
    <path d="M6 3h12v18l-2-1.4-2 1.4-2-1.4-2 1.4-2-1.4L6 21z" />
    <path d="M9 8h6M9 11.5h6M9 15h3.5" />
  </Icon>
);
export const DownloadIcon = (p: P) => (
  <Icon {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 7.5v8M8.5 12.5L12 16l3.5-3.5" />
  </Icon>
);
export const PaperclipIcon = (p: P) => (
  <Icon {...p}>
    <path d="M20 11.5l-7.8 7.8a5 5 0 0 1-7.1-7.1l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7l-8.4 8.4a1.7 1.7 0 0 1-2.4-2.4l7.6-7.6" />
  </Icon>
);
export const CheckIcon = (p: P) => (
  <Icon {...p}>
    <path d="M5 12.5l4.5 4.5L19 7.5" />
  </Icon>
);
export const ArchiveIcon = (p: P) => (
  <Icon {...p}>
    <rect x="3" y="4" width="18" height="5" rx="1.5" />
    <path d="M5 9v9.5a1.5 1.5 0 0 0 1.5 1.5h11a1.5 1.5 0 0 0 1.5-1.5V9M10 13h4" />
  </Icon>
);
export const ReplyIcon = (p: P) => (
  <Icon {...p}>
    <path d="M9.5 6L4 11.5 9.5 17" />
    <path d="M4.5 11.5H14a6 6 0 0 1 6 6V19" />
  </Icon>
);
export const BackIcon = (p: P) => (
  <Icon {...p}>
    <path d="M14.5 5.5L8 12l6.5 6.5" />
  </Icon>
);
export const SignOutIcon = (p: P) => (
  <Icon {...p}>
    <path d="M14 4.5H6.5A1.5 1.5 0 0 0 5 6v12a1.5 1.5 0 0 0 1.5 1.5H14" />
    <path d="M10 12h10M16.5 8.5L20 12l-3.5 3.5" />
  </Icon>
);
export const BellIcon = (p: P) => (
  <Icon {...p}>
    <path d="M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15z" />
    <path d="M10 20.5a2 2 0 0 0 4 0" />
  </Icon>
);
export const LockIcon = (p: P) => (
  <Icon {...p}>
    <rect x="5" y="10.5" width="14" height="10" rx="2.5" />
    <path d="M8 10.5V8a4 4 0 0 1 8 0v2.5" />
  </Icon>
);
