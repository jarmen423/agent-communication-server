/** One searchable thing in the ⌘K menu: a docs page, a docs section, or a command. */
export type SearchEntry = {
  kind: "page" | "section" | "action";
  /** Group heading in the menu ("Docs", "Sections", "Go to", "Actions"). */
  group: string;
  /** Page title (for sections, the page the section is on). */
  title: string;
  /** Section heading, for kind "section". */
  heading?: string;
  url: string;
  /** Plain text the query is matched against; also the source of snippets. */
  text: string;
  /** Only shown once the user types (keeps the empty menu short). */
  searchOnly?: boolean;
};
