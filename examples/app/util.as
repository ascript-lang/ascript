// Leaf module: exported consts + pure functions, no imports of its own.
export const TAU = 6.28318

export fn scale(value: number, factor: number): number {
  return value * factor
}

// References TAU in-file so the const is exercised here as well as by importers.
export fn turns(count: number): number {
  return scale(count, TAU)
}

export fn label(name: string, value: number): string {
  return `${name}=${value}`
}
