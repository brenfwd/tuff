import { lazy, Suspense, useEffect } from "react";
import { Route, Router, Switch, useLocation } from "wouter";
import { useHashLocation } from "./lib/hash";
import { Header } from "./components/Header";
import { Footer } from "./components/Footer";
import { SearchProvider } from "./components/SearchContext";
import Home from "./pages/Home";

const Docs = lazy(() => import("./pages/Docs"));
const NotFound = lazy(() => import("./pages/NotFound"));

function ScrollToTop() {
  const [path] = useLocation();
  useEffect(() => {
    if (!location.hash.includes("?h=")) window.scrollTo({ top: 0 });
  }, [path]);
  return null;
}

export default function App() {
  return (
    <Router hook={useHashLocation}>
      <SearchProvider>
        <a href="#main" className="skip-link" onClick={(e) => { e.preventDefault(); document.getElementById("main")?.focus(); }}>
          Skip to content
        </a>
        <ScrollToTop />
        <Header />
        <Suspense fallback={<div className="min-h-[70vh]" />}>
          <Switch>
            <Route path="/" component={Home} />
            <Route path="/docs" component={Docs} />
            <Route path="/docs/*" component={Docs} />
            <Route component={NotFound} />
          </Switch>
        </Suspense>
        <Footer />
      </SearchProvider>
    </Router>
  );
}
