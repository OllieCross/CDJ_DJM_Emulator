/// Keep receiving a drag's pointer events outside the element. Can fail for
/// pointers the browser doesn't consider active (synthetic events); the drag
/// still works inside the element then.
export function capture(el: Element, e: PointerEvent) {
  try {
    el.setPointerCapture(e.pointerId);
  } catch {
    // Not capturable; carry on without.
  }
}
