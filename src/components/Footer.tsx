import { Link } from "wouter";
import { Logo } from "./Logo";
import { REPO_URL } from "../lib/site";

export function Footer() {
  const cols = [
    { title: "Get started", links: [["/docs/introduction", "Introduction"], ["/docs/installing", "Installing"], ["/docs/running", "Running"], ["/docs/settings", "Settings"]] },
    { title: "Build", links: [["/docs/plugins", "WebAssembly plugins"], ["/docs/features", "Built-in features"], ["/docs/data-packs", "Data packs"]] },
    { title: "Project", links: [["/blog", "Blog"], [REPO_URL, "Source code"], [`${REPO_URL}/issues`, "Issues"], [`${REPO_URL}/blob/main/CONTRIBUTING.md`, "Contributing"], ["/docs/license", "License"]] },
  ];
  return (
    <footer className="relative border-t border-line bg-bg-sunken">
      <div className="mx-auto grid max-w-7xl gap-10 px-4 py-14 sm:px-6 md:grid-cols-[1.4fr_repeat(3,1fr)]">
        <div className="max-w-xs">
          <Logo />
          <p className="mt-4 text-sm leading-6 text-fg-muted">A Minecraft Java Edition server written in Rust, for game version 26.3. Not affiliated with Mojang or Microsoft.</p>
        </div>
        {cols.map((c) => (
          <div key={c.title}>
            <h2 className="text-xs font-semibold uppercase tracking-[0.12em] text-fg-subtle">{c.title}</h2>
            <ul className="mt-4 space-y-2.5">
              {c.links.map(([href, label]) => (
                <li key={href}>
                  {href.startsWith("http") ? (
                    <a href={href} target="_blank" rel="noopener noreferrer" className="text-sm text-fg-muted transition hover:text-fg">{label}</a>
                  ) : (
                    <Link href={href} className="text-sm text-fg-muted transition hover:text-fg">{label}</Link>
                  )}
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
      <div className="border-t border-line">
        <p className="mx-auto max-w-7xl px-4 py-6 text-xs text-fg-subtle sm:px-6">Server licensed AGPL-3.0-only. Plugin SDK, ABI and examples licensed MIT. Minecraft is a trademark of Mojang Synergies AB.</p>
      </div>
    </footer>
  );
}
