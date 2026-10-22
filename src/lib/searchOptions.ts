export const searchOptions = {
  fields: ["heading", "pageTitle", "text"],
  storeFields: ["route", "page", "pageTitle", "anchor", "heading", "group", "snippet"],
  searchOptions: { boost: { heading: 3, pageTitle: 2 }, prefix: true, fuzzy: 0.2 },
};
