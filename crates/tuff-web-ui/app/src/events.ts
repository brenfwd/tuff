export type Handler = (event: string, data: string) => void;

export function subscribe(topics: string[], events: string[], handler: Handler): () => void {
  const source = new EventSource(`/admin/events?topics=${encodeURIComponent(topics.join(","))}`, {
    withCredentials: true,
  });
  for (const name of [...events, "resync"]) {
    source.addEventListener(name, (e) => handler(name, (e as MessageEvent).data));
  }
  return () => source.close();
}
