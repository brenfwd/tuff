import { createContext, lazy, Suspense, useCallback, useContext, useEffect, useState, type ReactNode } from "react";

const SearchPalette = lazy(() => import("./SearchPalette"));
const Ctx = createContext<{ open: () => void }>({ open: () => {} });
export const useSearch = () => useContext(Ctx);

export function SearchProvider({ children }: { children: ReactNode }) {
  const [isOpen, setOpen] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const open = useCallback(() => {
    setLoaded(true);
    setOpen(true);
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.key === "k" || e.key === "K") && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setLoaded(true);
        setOpen((o) => !o);
      } else if (e.key === "/" && !isOpen && !(e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement)) {
        e.preventDefault();
        open();
      }
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [isOpen, open]);
  return (
    <Ctx.Provider value={{ open }}>
      {children}
      {loaded && (
        <Suspense fallback={null}>
          <SearchPalette open={isOpen} onOpenChange={setOpen} />
        </Suspense>
      )}
    </Ctx.Provider>
  );
}
