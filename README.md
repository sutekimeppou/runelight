# Runelight

A macOS app launcher with fuzzy search and quicklinks, built with [Rune](https://github.com/anomalyco/rune).

## Config

Create `~/.config/runelight/config.json` (or `$XDG_CONFIG_HOME/runelight/config.json`):

```json
{
  "hotkey": "alt+space",
  "quicklinks": [
    { "name": "GitHub", "url": "https://github.com" },
    { "name": "Notes", "url": "obsidian://open?vault=Notes" }
  ],
  "fallbacks": [
    { "name": "Search Google", "url": "https://www.google.com/search?q={query}" },
    { "name": "Ask ChatGPT", "url": "codex://new?prompt={query}" }
  ]
}
```

Config reloads each time the launcher opens.
An optional `config.local.json` alongside it adds links and overrides the hotkey.

## License

MIT licensed. See [LICENSE](LICENSE).
