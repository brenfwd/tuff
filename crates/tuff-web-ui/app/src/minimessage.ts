/** MiniMessage helpers for the panel: a CodeMirror highlighting mode and a
 * flattening of server-parsed text components into styled runs. */
import { StreamLanguage, type StreamParser } from "@codemirror/language";

interface MmState {
  inTag: boolean;
  quote: string | null;
}

export const miniMessageParser: StreamParser<MmState> = {
  name: "minimessage",
  startState: () => ({ inTag: false, quote: null }),
  token(stream, state) {
    if (state.quote) {
      while (!stream.eol()) {
        const c = stream.next();
        if (c === "\\") stream.next();
        else if (c === state.quote) {
          state.quote = null;
          break;
        }
      }
      return "string";
    }
    if (state.inTag) {
      if (stream.eat(">")) {
        state.inTag = false;
        return "bracket";
      }
      const c = stream.peek();
      if (c === "'" || c === '"') {
        state.quote = stream.next() ?? null;
        return "string";
      }
      if (stream.eat(":")) return "punctuation";
      if (stream.match(/^#[0-9a-fA-F]{6}/)) return "number";
      stream.eatWhile(/[^:>'"]/);
      return "attributeValue";
    }
    if (stream.match("\\<") || stream.match("\\\\")) return "escape";
    if (stream.match(/^<\/?[!?#]?[A-Za-z0-9_.-]+/)) {
      state.inTag = true;
      return "tagName";
    }
    if (stream.match(/^[&§](?:[0-9a-fk-orA-FK-OR]|#[0-9a-fA-F]{6})/)) return "keyword";
    stream.next();
    stream.eatWhile(/[^<\\&§]/);
    return null;
  },
};

export const miniMessage = () => StreamLanguage.define(miniMessageParser);

const NAMED: Record<string, string> = {
  black: "#000000",
  dark_blue: "#0000AA",
  dark_green: "#00AA00",
  dark_aqua: "#00AAAA",
  dark_red: "#AA0000",
  dark_purple: "#AA00AA",
  gold: "#FFAA00",
  gray: "#AAAAAA",
  dark_gray: "#555555",
  blue: "#5555FF",
  green: "#55FF55",
  aqua: "#55FFFF",
  red: "#FF5555",
  light_purple: "#FF55FF",
  yellow: "#FFFF55",
  white: "#FFFFFF",
};

/** The CSS colour for a component colour string. */
export function cssColor(color: string): string | undefined {
  if (/^#[0-9a-fA-F]{6}$/.test(color)) return color.toUpperCase();
  return NAMED[color];
}

export interface Style {
  color?: string;
  bold?: boolean;
  italic?: boolean;
  underlined?: boolean;
  strikethrough?: boolean;
  obfuscated?: boolean;
  font?: string;
  hover?: string;
  click?: string;
}

export interface Run {
  text: string;
  style: Style;
}

type Json = string | number | boolean | null | Json[] | { [key: string]: Json };

function str(v: Json | undefined): string | undefined {
  return typeof v === "string" ? v : undefined;
}

/** Plain text of a component, as the client would show a hover tooltip. */
export function plain(component: Json): string {
  return flatten(component)
    .map((r) => r.text)
    .join("");
}

/** A component's text as styled runs, children inheriting their parent's
 * style. Translations, keybinds, scores, selectors and NBT show their key
 * or source in brackets, since only the client or a live world knows them. */
export function flatten(component: Json, inherited: Style = {}, depth = 0): Run[] {
  if (depth > 64) return [];
  if (typeof component === "string") return component ? [{ text: component, style: inherited }] : [];
  if (Array.isArray(component)) {
    if (component.length === 0) return [];
    const [first, ...rest] = component;
    const head = flatten(first, inherited, depth + 1);
    const style = head.length > 0 ? head[0].style : inherited;
    return [...head, ...rest.flatMap((c) => flatten(c, style, depth + 1))];
  }
  if (component === null || typeof component !== "object") return [];
  const c = component;
  const style: Style = { ...inherited };
  const color = str(c.color);
  if (color) style.color = cssColor(color) ?? style.color;
  for (const key of ["bold", "italic", "underlined", "strikethrough", "obfuscated"] as const) {
    if (typeof c[key] === "boolean") style[key] = c[key] as boolean;
  }
  if (str(c.font)) style.font = str(c.font);
  const hover = c.hover_event;
  if (hover && typeof hover === "object" && !Array.isArray(hover)) {
    const value = hover.value ?? hover.name ?? hover.id;
    style.hover = value === undefined ? undefined : plain(value);
  }
  const click = c.click_event;
  if (click && typeof click === "object" && !Array.isArray(click)) {
    const action = str(click.action) ?? "";
    const value = click.url ?? click.command ?? click.value ?? click.page ?? click.id;
    style.click = `${action}: ${value === undefined ? "" : String(value)}`;
  }
  const own: Run[] = [];
  const translate = str(c.translate);
  if (translate !== undefined) {
    const args = Array.isArray(c.with) ? c.with.map((w) => plain(w)) : [];
    own.push({ text: str(c.fallback) ?? (args.length ? `${translate}(${args.join(", ")})` : translate), style });
  } else if (str(c.keybind) !== undefined) {
    own.push({ text: `[${str(c.keybind)}]`, style });
  } else if (c.score && typeof c.score === "object" && !Array.isArray(c.score)) {
    own.push({ text: `[score ${str(c.score.name) ?? ""} ${str(c.score.objective) ?? ""}]`, style });
  } else if (str(c.selector) !== undefined) {
    own.push({ text: `[${str(c.selector)}]`, style });
  } else if (str(c.nbt) !== undefined) {
    own.push({ text: `[nbt ${str(c.nbt)}]`, style });
  } else if (str(c.sprite) !== undefined) {
    own.push({ text: `[${str(c.sprite)}]`, style });
  } else if (c.player !== undefined) {
    own.push({ text: "[head]", style });
  } else if (str(c.text)) {
    own.push({ text: str(c.text) as string, style });
  }
  const extra = Array.isArray(c.extra) ? c.extra.flatMap((e) => flatten(e, style, depth + 1)) : [];
  return [...own, ...extra];
}

/** Inline CSS for a run. */
export function runCss(style: Style): Record<string, string> {
  const css: Record<string, string> = {};
  if (style.color) css.color = style.color;
  if (style.bold) css.fontWeight = "bold";
  if (style.italic) css.fontStyle = "italic";
  const lines = [style.underlined && "underline", style.strikethrough && "line-through"].filter(Boolean);
  if (lines.length) css.textDecoration = lines.join(" ");
  if (style.obfuscated) css.filter = "blur(2px)";
  return css;
}
