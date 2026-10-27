/** The ten digits of an Indian mobile from whatever was typed, or null. */
export function normalizePhone(raw: string): string | null {
  let digits = raw.replace(/\D/g, '');
  if (digits.length === 12 && digits.startsWith('91')) digits = digits.slice(2);
  if (digits.length === 11 && digits.startsWith('0')) digits = digits.slice(1);
  return digits.length === 10 && /^[6-9]/.test(digits) ? digits : null;
}

/** `98765 43210`. */
export function formatPhone(phone: string): string {
  return phone.length === 10 ? `${phone.slice(0, 5)} ${phone.slice(5)}` : phone;
}


/** What to call a signed-in person: their name, else their number. */
export function displayName(user: { name: string | null; phone: string } | null): string {
  if (!user) return '';
  return user.name?.trim() || formatPhone(user.phone);
}
