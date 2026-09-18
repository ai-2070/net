/**
 * The Three.js half: state in, meshes out.
 *
 * It reads the store and nothing else — no transport, no protocol. The
 * reason that matters is the one the store design is built on: a render
 * loop runs at 60 Hz and must never be the thing that decides what is
 * visible. Here it decides only how it looks.
 */

import * as THREE from 'three';

import { ARENA } from './game.js';

const HULL_COLOURS = [0x4fc3f7, 0xffb74d, 0x81c784, 0xe57373, 0xba68c8];

export function createScene(canvas) {
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: true });
  renderer.setPixelRatio(Math.min(globalThis.devicePixelRatio ?? 1, 2));

  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0x0b1020);
  scene.fog = new THREE.Fog(0x0b1020, 30, 70);

  // Low and close: the arena should fill the frame, and a ship should
  // be large enough that its hull bar reads at a glance.
  const camera = new THREE.PerspectiveCamera(50, 1, 0.1, 200);
  camera.position.set(0, 13, 17);
  camera.lookAt(0, 1, -1);

  scene.add(new THREE.HemisphereLight(0x9fc5ff, 0x101426, 1.1));
  const sun = new THREE.DirectionalLight(0xffffff, 1.4);
  sun.position.set(8, 18, 10);
  scene.add(sun);

  const water = new THREE.Mesh(
    new THREE.PlaneGeometry(ARENA * 2 + 4, ARENA * 2 + 4),
    new THREE.MeshStandardMaterial({ color: 0x13203d, roughness: 0.85, metalness: 0.1 }),
  );
  water.rotation.x = -Math.PI / 2;
  scene.add(water);

  const grid = new THREE.GridHelper(ARENA * 2, ARENA, 0x33507f, 0x1d2f52);
  grid.position.y = 0.01;
  scene.add(grid);

  const waypoint = new THREE.Mesh(
    new THREE.TorusGeometry(0.9, 0.12, 8, 24),
    new THREE.MeshBasicMaterial({ color: 0xffe082 }),
  );
  waypoint.rotation.x = -Math.PI / 2;
  waypoint.visible = false;
  scene.add(waypoint);

  /** One group per ship, keyed by the peer that owns it. */
  const fleet = new Map();

  function buildShip(colourIndex, mine) {
    const group = new THREE.Group();
    const colour = HULL_COLOURS[colourIndex % HULL_COLOURS.length];
    const body = new THREE.Mesh(
      new THREE.CapsuleGeometry(0.6, 1.9, 4, 12),
      new THREE.MeshStandardMaterial({ color: colour, roughness: 0.4, metalness: 0.3 }),
    );
    body.rotation.x = Math.PI / 2;
    body.position.y = 0.6;
    group.add(body);

    const prow = new THREE.Mesh(
      new THREE.ConeGeometry(0.46, 1.2, 12),
      new THREE.MeshStandardMaterial({ color: 0xf5f5f5, roughness: 0.5 }),
    );
    prow.rotation.x = Math.PI / 2;
    prow.position.set(0, 0.6, 1.6);
    group.add(prow);

    const hull = new THREE.Mesh(
      new THREE.BoxGeometry(2, 0.18, 0.18),
      new THREE.MeshBasicMaterial({ color: 0x7cffb2 }),
    );
    hull.position.y = 2;
    group.add(hull);

    if (mine) {
      const marker = new THREE.Mesh(
        new THREE.RingGeometry(1.35, 1.6, 28),
        new THREE.MeshBasicMaterial({ color: 0xffffff, side: THREE.DoubleSide }),
      );
      marker.rotation.x = -Math.PI / 2;
      marker.position.y = 0.05;
      group.add(marker);
    }
    return { group, hull };
  }

  /**
   * Reconcile the scene graph with the state.
   *
   * Ships are added and removed here rather than rebuilt: a snapshot
   * that did not change a ship must not churn its mesh, which is the
   * same reason the store shares unchanged subtrees.
   */
  function apply(state, self) {
    for (const [id, ship] of Object.entries(state.ships)) {
      let entry = fleet.get(id);
      if (entry === undefined) {
        entry = buildShip(ship.colour, id === self);
        scene.add(entry.group);
        fleet.set(id, entry);
      }
      entry.group.position.set(ship.x, 0, ship.z);
      entry.group.rotation.y = ship.heading;
      entry.hull.scale.x = Math.max(ship.hull, 0) / 100;
      entry.hull.material.color.setHex(ship.hull > 50 ? 0x7cffb2 : 0xff7c7c);
    }
    for (const [id, entry] of [...fleet]) {
      if (state.ships[id] !== undefined) continue;
      scene.remove(entry.group);
      fleet.delete(id);
    }
    if (state.waypoint === null) {
      waypoint.visible = false;
    } else {
      waypoint.visible = true;
      waypoint.position.set(state.waypoint.x, 0.2, state.waypoint.z);
    }
  }

  function resize(width, height) {
    renderer.setSize(width, height, false);
    camera.aspect = width / Math.max(height, 1);
    camera.updateProjectionMatrix();
  }

  function render() {
    renderer.render(scene, camera);
  }

  /** What is on screen, for a check that does not need pixels. */
  function readback() {
    const ships = {};
    for (const [id, entry] of fleet) {
      ships[id] = {
        x: Number(entry.group.position.x.toFixed(3)),
        z: Number(entry.group.position.z.toFixed(3)),
        hull: Number(entry.hull.scale.x.toFixed(3)),
      };
    }
    return { ships, waypoint: waypoint.visible, drawn: renderer.info.render.frame };
  }

  return { apply, resize, render, readback, dispose: () => renderer.dispose() };
}
