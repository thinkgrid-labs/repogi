export function validateLogin(user: string, password: string): boolean {
  return validateUserName(user) && password.length >= 12;
}

export function validateUserName(user: string): boolean {
  return /^[a-z]+$/.test(user);
}
