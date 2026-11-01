import { add, multiply, shout } from '../lib/index.js';
import mul from '../lib/math.js';
import * as math from '../lib/math.js';
import { add as plus } from '@lib/math.js';
import { Circle, Shape } from '../lib/shapes.js';
import { formatName } from './helpers';
import { mode } from './config';
import { missing } from './nowhere.js';
import express from 'express';

export function run() {
  const sum = add(1, 2);
  const product = multiply(2, 3);
  const product2 = mul(4, 5);
  const viaNamespace = math.add(6, 7);
  const viaAlias = plus(8, 9);
  const circle = new Circle(1);
  const unit = Shape.unit();
  return shout(formatName(`${sum} ${product} ${product2} ${viaNamespace} ${viaAlias} ${circle.radius} ${unit} ${mode}`));
}

export function shadowed(add, { multiply }, [mul] = [], ...rest) {
  return add(1) + multiply(2) + mul(3) + rest.length;
}

export function reassigned() {
  let pick = formatName;
  pick = String;
  return pick('x');
}

export async function lazy() {
  const loaded = await import('../lib/math.js');
  return loaded.add(1, 2) + missing() + express();
}

export class Square extends Shape {}
