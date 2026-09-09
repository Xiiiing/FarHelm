let unsavedDrafts = 0
export function setUnsavedDraftCount(count: number) { unsavedDrafts = Math.max(0, count) }
export function hasUnsavedDrafts() { return unsavedDrafts > 0 }
