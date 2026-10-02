// Retain only admitted File references; bytes are read only when submitting.
export function imageFile(file) {
  return /^image\//.test(file.type) || /\.(png|jpe?g|webp|gif)$/i.test(file.name);
}

export function admitDraftFiles(files, historical = []) {
  if (!Number.isInteger(files.length) || files.length < 0 || files.length + historical.length > 4) {
    throw new Error('At most 4 attachments per turn');
  }
  let textBytes = 0;
  for (const attachment of historical) {
    if (attachment.type === 'text') {
      if (attachment.text.length > 256 * 1024) throw new Error('Embedded text exceeds 256 KiB');
      textBytes += new TextEncoder().encode(attachment.text).length;
    }
  }
  for (let index = 0; index < files.length; index++) {
    const file = files[index];
    if (!Number.isSafeInteger(file.size) || file.size < 0) throw new Error('Invalid attachment size');
    if (imageFile(file)) {
      if (file.size > 2 * 1024 * 1024) throw new Error('An image exceeds 2 MiB; resize it first');
    } else textBytes += file.size;
  }
  if (textBytes > 256 * 1024) throw new Error('Embedded text exceeds 256 KiB');
  return Array.from(files);
}
