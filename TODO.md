# Backlog

- [ ] Add optional Jev (TypeSafe, https://typesafe.ai) assistance to choose ours or theirs for individual merge differences, not an entire file at once.
  - Manual merging must remain available without an API key.
  - A configured API key must not activate Jev automatically. Using Jev to make merge decisions requires explicit user opt-in.
  - Jev choices must remain reviewable and must not save or submit the merged result without explicit user confirmation.
- [ ] Make the final merge confirmation flow explicit: offer confirmation once every difference has a choice, and let the user invoke confirmation manually. Unresolved differences must be identified rather than silently treated as accepted.
