export function LogoMark({ className = "h-7 w-7" }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden="true">
      <path d="M16 2 29 9.5 16 17 3 9.5Z" fill="var(--ember)" />
      <path d="M3 9.5 16 17v13L3 22.5Z" fill="currentColor" opacity="0.55" />
      <path d="M29 9.5 16 17v13l13-7.5Z" fill="currentColor" opacity="0.85" />
      <path d="M9 9.5 16 5.5l7 4-7 4Z" fill="#fff" opacity="0.18" />
    </svg>
  );
}

export function Logo() {
  return (
    <span className="flex items-center gap-2.5">
      <LogoMark />
      <span className="font-display text-xl font-bold tracking-tight">tuff</span>
    </span>
  );
}
