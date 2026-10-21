import { Link } from "wouter";
import { useSeo } from "../lib/useSeo";

export default function NotFound() {
  useSeo({ title: "Page not found - tuff" });
  return (
    <main id="main" tabIndex={-1} className="mx-auto flex min-h-[60vh] max-w-xl flex-col items-center justify-center px-4 py-24 text-center">
      <p className="font-mono text-sm text-ember-text">404</p>
      <h1 className="mt-3 font-display text-4xl font-bold tracking-tight">This chunk was never generated.</h1>
      <p className="mt-4 text-fg-muted">The page you were looking for does not exist.</p>
      <div className="mt-8 flex gap-3">
        <Link href="/" className="rounded-xl bg-fg px-5 py-2.5 text-sm font-semibold text-bg">Home</Link>
        <Link href="/docs/introduction" className="rounded-xl border border-line px-5 py-2.5 text-sm font-semibold">Documentation</Link>
      </div>
    </main>
  );
}
