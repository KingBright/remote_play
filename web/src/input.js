// The canvas uses object-fit: contain. Ignore letterbox bars on pointer-down;
// callers clamp captured moves/releases so a drag can end outside the picture.
export function canvasPoint(x, y, rect, width, height) {
  if (rect.width <= 0 || rect.height <= 0 || width <= 0 || height <= 0) return null;
  const displayWidth = Math.min(rect.width, rect.height * (width / height));
  const displayHeight = Math.min(rect.height, rect.width * (height / width));
  return [
    (x - rect.left - (rect.width - displayWidth) / 2) / displayWidth,
    (y - rect.top - (rect.height - displayHeight) / 2) / displayHeight
  ];
}
