# Blog posts

Each post is `content/blog/<slug>.md`; the slug is the lowercase kebab-case file name. Frontmatter:

| Field | Required | Meaning |
| --- | --- | --- |
| `title` | yes | Post title. Quote it if it contains `: ` (e.g. `title: "Items: a story"`), or YAML parsing fails. |
| `description` | yes | One-sentence summary for cards, meta tags and feeds. |
| `date` | yes | Publication date, `YYYY-MM-DD`. The updated date comes from git. |
| `author` | yes | The human author's display name, shown on the post, the index, the OG image, JSON-LD and feeds. |
| `ai` | no | Short AI attribution, e.g. `ai: "Drafted with Claude Fable 5.1 and edited by the author."`. Rendered as a footnote after the post and in feeds. Omit when no AI was used. |
| `tags` | no | List of strings. |
| `image` | no | Custom social image path or URL; otherwise one is generated. |
| `draft` | no | `true` hides the post from production builds. |
