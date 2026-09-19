/**
 * The Three.js half: state in, meshes out.
 *
 * It reads the store and nothing else — no transport, no protocol. The
 * reason that matters is the one the store design is built on: a render
 * loop runs at 60 Hz and must never be the thing that decides what is
 * visible. Here it decides only how it looks.
 */

import * as THREE from 'three';

import { bindEntities } from '../dist/three/index.js';

import { ARENA } from './game.js';

const HULL_COLOURS = [0x4fc3f7, 0xffb74d, 0x81c784, 0xe57373, 0xba68c8];

/**
 * Put a ship's group where its state says it is.
 *
 * One function, used by BOTH `create` and `update`, so an initial
 * render and a later one cannot drift apart.
 */
function place(group, ship) {
  group.position.set(ship.x, 0, ship.z);
  group.rotation.y = ship.heading;
  const hull = group.userData.hull;
  hull.scale.x = Math.max(ship.hull, 0) / 100;
  hull.material.color.setHex(ship.hull > 50 ? 0x7cffb2 : 0xff7c7c);
}

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

  /**
   * One group per ship, keyed by the peer that owns it.
   *
   * Held by `@net-mesh/browser/three` now, not by this file: the
   * add/update/remove reconciliation was the same twenty lines every
   * game writes, and the two things it is easy to get wrong —
   * rebuilding a ship nobody moved, and leaking one that left — are
   * the two the binding exists to stop.
   */
  let fleet = null;
  let bound = null;

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
  /**
   * Attach the scene to a store.
   *
   * `self` is this page's own node, so its ship gets the ring marker.
   */
  function attach(store, self) {
    bound = bindEntities({
      store,
      // The graph is `THREE.Scene` itself: `add`/`remove` is the whole
      // structural surface the binding asks for.
      scene,
      select: state => state.ships,
      binding: {
        create: (ship, id) => {
          const entry = buildShip(ship.colour, id === self);
          entry.group.name = id;
          // The binding adds the returned object to the scene, so
          // what it gets back is the group and the hull bar rides
          // along on it.
          entry.group.userData.hull = entry.hull;
          // CREATE ALSO PLACES IT. The binding skips an entity whose
          // reference did not change, so a ship that is created and
          // then never moves is never updated either — it would sit
          // at the geometry's defaults (origin, heading 0, full
          // hull) however far from them its actual state is. A
          // review probe measured exactly that: expected
          // {x:-6,z:3,heading:1,hull:0.75}, observed {0,0,0,1}.
          // `create` returns a rendered object, not a blank one.
          place(entry.group, ship);
          return entry.group;
        },
        update: place,
        remove: group => {
          // Ours to do: Three.js leaks geometries and materials, and
          // only this file knows none of these are shared.
          group.traverse(child => {
            child.geometry?.dispose?.();
            child.material?.dispose?.();
          });
        },
      },
    });
    fleet = bound;
    return bound;
  }

  /** The parts of the view that are not entities. */
  function apply(state) {
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
    for (const child of scene.children) {
      const hull = child.userData?.hull;
      if (hull === undefined) continue;
      ships[child.name || child.uuid] = {
        x: Number(child.position.x.toFixed(3)),
        z: Number(child.position.z.toFixed(3)),
        hull: Number(hull.scale.x.toFixed(3)),
      };
    }
    return {
      ships,
      count: bound === null ? 0 : bound.size,
      waypoint: waypoint.visible,
      drawn: renderer.info.render.frame,
    };
  }

  return {
    attach,
    apply,
    resize,
    render,
    readback,
    dispose: () => {
      bound?.dispose();
      renderer.dispose();
    },
  };
}
