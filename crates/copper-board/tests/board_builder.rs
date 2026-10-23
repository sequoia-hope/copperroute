#![allow(dead_code)]

use std::collections::BTreeMap;

use copper_board::prelude::*;
use copper_geometry::{
    IntBox, IntOctagon, IntPoint, IntVector, Point, Polyline, PolylineShapeRef, Shape, TileShape,
    Vector,
};

pub const BOUNDING_BOX: IntBox = IntBox {
    ll: IntPoint {
        x: -10_000,
        y: -10_000,
    },
    ur: IntPoint {
        x: 10_000,
        y: 10_000,
    },
};

pub const WIDE_CLEARANCE_CLASS: usize = 2;

pub struct BoardFixture {
    pub library: BoardLibrary,
    pub components: Components,
    pub rules: BoardRules,
    pub bounding_box: IntBox,
    pub items: BTreeMap<ItemId, Item>,
    pub manager: SearchTreeManager,
}

impl BoardFixture {
    pub fn new() -> BoardFixture {
        BoardFixture::with_angle(AngleRestriction::FortyFiveDegree)
    }

    pub fn with_angle(angle: AngleRestriction) -> BoardFixture {
        let layer_structure = layers();
        let mut clearance_matrix = ClearanceMatrix::get_default_instance(&layer_structure, 200);
        assert!(clearance_matrix.append_class("wide"));
        clearance_matrix.set_value_on_all_layers(2, 1, 600);
        clearance_matrix.set_value_on_all_layers(2, 2, 800);
        let mut rules = BoardRules::new(layers(), clearance_matrix);
        rules.trace_angle_restriction = angle;

        let mut padstacks = Padstacks::new(layers());
        let smd = padstacks.add(
            "smd",
            vec![
                Some(Shape::Tile(TileShape::Box(IntBox::from_coords(
                    -50, -50, 50, 50,
                )))),
                None,
            ],
            false,
            false,
        );
        let through_shape = Shape::Tile(TileShape::Octagon(IntOctagon::new(
            -70, -70, 70, 70, -140, 140, -140, 140,
        )));
        let through = padstacks.add(
            "thru",
            vec![Some(through_shape.clone()), Some(through_shape)],
            true,
            false,
        );

        let mut packages = Packages::new();
        let package = packages.add(
            "pkg",
            vec![
                PackagePin::new("P1", smd, IntVector::new(-500, 0).into(), 0.0),
                PackagePin::new("P2", through, IntVector::new(500, 0).into(), 0.0),
            ],
            None,
            None,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            true,
        );
        let library = BoardLibrary::new(padstacks, packages);

        let mut components = Components::new();
        components.add_with_generated_name(Some(Point::new(0, 0)), 0.0, true, package);

        let mut items = BTreeMap::new();
        items.insert(
            ItemId(1),
            Item::BoardOutline(BoardOutline::new(
                ItemHeader::new(ItemId(1), Vec::new(), 0, 0, FixedState::SystemFixed),
                Vec::new(),
            )),
        );
        items.insert(
            ItemId(2),
            Item::Pin(Pin::new(
                ItemHeader::new(ItemId(2), vec![1], 1, 1, FixedState::Unfixed),
                0,
            )),
        );
        items.insert(
            ItemId(3),
            Item::Pin(Pin::new(
                ItemHeader::new(ItemId(3), vec![1], 1, 1, FixedState::Unfixed),
                1,
            )),
        );
        items.insert(ItemId(4), Item::Trace(signal_trace()));
        items.insert(ItemId(5), Item::Trace(wide_trace()));

        BoardFixture {
            library,
            components,
            rules,
            bounding_box: BOUNDING_BOX,
            items,
            manager: SearchTreeManager::new(),
        }
    }

    pub fn ctx(&self) -> ItemCtx<'_> {
        ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        }
    }

    pub fn insert_all(&mut self) {
        let mut items = std::mem::take(&mut self.items);
        let ctx = ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        for item in items.values_mut() {
            self.manager.insert(item, &ctx);
        }
        self.items = items;
    }

    pub fn build_autoroute_tree(&mut self, clearance_class_index: usize) -> TreeId {
        let mut items = std::mem::take(&mut self.items);
        let ctx = ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        let mut refs: Vec<&mut Item> = items.values_mut().rev().collect();
        let id = self
            .manager
            .get_autoroute_tree(clearance_class_index, &mut refs, &ctx)
            .id();
        drop(refs);
        self.items = items;
        id
    }

    pub fn items_in_board_order(&mut self) -> Vec<&mut Item> {
        self.items.values_mut().rev().collect()
    }

    pub fn tree(&self, id: TreeId) -> &ShapeSearchTree {
        self.manager
            .trees()
            .find(|tree| tree.id() == id)
            .expect("the fixture only asks for trees it built")
    }
}

impl Default for BoardFixture {
    fn default() -> Self {
        Self::new()
    }
}

pub fn layers() -> LayerStructure {
    LayerStructure::new(vec![Layer::new("front", true), Layer::new("back", true)])
}

pub fn signal_trace() -> PolylineTrace {
    PolylineTrace::new(
        ItemHeader::new(ItemId(4), vec![1], 1, 0, FixedState::Unfixed),
        Polyline::from_points(&[
            Point::new(-500, 0),
            Point::new(0, 0),
            Point::new(0, 400),
            Point::new(500, 400),
        ]),
        0,
        30,
        None,
    )
}

pub fn wide_trace() -> PolylineTrace {
    PolylineTrace::new(
        ItemHeader::new(ItemId(5), vec![2], 2, 0, FixedState::Unfixed),
        Polyline::from_points(&[
            Point::new(-800, 300),
            Point::new(-800, 900),
            Point::new(300, 900),
        ]),
        0,
        40,
        None,
    )
}

#[test]
fn the_fixture_matches_the_java_driver_board() {
    let f = BoardFixture::new();
    let ctx = f.ctx();
    let describe = |id: u32| {
        let item = &f.items[&ItemId(id)];
        (
            item.net_nos().to_vec(),
            item.header().clearance_class(),
            item.first_layer(&ctx),
            item.last_layer(&ctx),
            item.tile_shape_count(&ctx),
            item.bounding_box(&ctx),
        )
    };
    assert_eq!(
        describe(2),
        (
            vec![1],
            1,
            0,
            0,
            1,
            IntBox::from_coords(-550, -50, -450, 50)
        )
    );
    assert_eq!(
        describe(3),
        (vec![1], 1, 0, 1, 2, IntBox::from_coords(430, -70, 570, 70))
    );
    assert_eq!(
        describe(4),
        (
            vec![1],
            1,
            0,
            0,
            3,
            IntBox::from_coords(-530, -30, 530, 430)
        )
    );
    assert_eq!(
        describe(5),
        (
            vec![2],
            2,
            0,
            0,
            2,
            IntBox::from_coords(-840, 260, 340, 940)
        )
    );
    assert_eq!(f.items[&ItemId(1)].tile_shape_count(&ctx), 0);
}

pub struct AreaFixture {
    pub library: BoardLibrary,
    pub components: Components,
    pub rules: BoardRules,
    pub bounding_box: IntBox,
    pub items: BTreeMap<ItemId, Item>,
    pub manager: SearchTreeManager,
}

impl AreaFixture {
    pub fn new() -> AreaFixture {
        let layers = || {
            LayerStructure::new(vec![
                Layer::new("front", true),
                Layer::new("inner", true),
                Layer::new("back", true),
            ])
        };
        let clearance_matrix = ClearanceMatrix::get_default_instance(&layers(), 200);
        let mut rules = BoardRules::new(layers(), clearance_matrix);
        rules.set_hole_clearance(300);

        let mut padstacks = Padstacks::new(layers());
        let pad = Shape::Tile(TileShape::Box(IntBox::from_coords(-80, -80, 80, 80)));
        let via_pad = padstacks.add("via", vec![Some(pad.clone()), None, Some(pad)], true, false);
        let library = BoardLibrary::new(padstacks, Packages::new());

        let mut items = BTreeMap::new();
        items.insert(
            ItemId(1),
            Item::BoardOutline(BoardOutline::new(
                ItemHeader::new(ItemId(1), Vec::new(), 1, 0, FixedState::SystemFixed),
                vec![PolylineShapeRef::Polygon(
                    copper_geometry::PolygonShape::from_points(&[
                        Point::new(-3000, -2000),
                        Point::new(3000, -2000),
                        Point::new(3000, 2000),
                        Point::new(-3000, 2000),
                    ]),
                )],
            )),
        );
        items.insert(
            ItemId(2),
            Item::Via(Via::new(
                ItemHeader::new(ItemId(2), vec![1], 1, 0, FixedState::Unfixed),
                via_pad,
                Point::new(1000, 0),
                true,
            )),
        );
        let l_shape = copper_geometry::Area::Shape(Shape::Polygon(
            copper_geometry::PolygonShape::from_points(&[
                Point::new(0, 0),
                Point::new(2000, 0),
                Point::new(2000, 1000),
                Point::new(1000, 1000),
                Point::new(1000, 2000),
                Point::new(0, 2000),
            ]),
        ));
        items.insert(
            ItemId(3),
            Item::ObstacleArea(ObstacleArea::new(
                ItemHeader::new(ItemId(3), Vec::new(), 1, 0, FixedState::Unfixed),
                ObstacleAreaData::new(l_shape, 0, Vector::ZERO, 0.0, false, None),
            )),
        );
        let square = copper_geometry::Area::Shape(Shape::Tile(TileShape::Box(
            IntBox::from_coords(-2000, -1500, -1000, -500),
        )));
        items.insert(
            ItemId(4),
            Item::ConductionArea(ConductionArea::new(
                ItemHeader::new(ItemId(4), vec![2], 1, 0, FixedState::Unfixed),
                ObstacleAreaData::new(square, 2, Vector::ZERO, 0.0, false, None),
                true,
            )),
        );

        let mut fixture = AreaFixture {
            library,
            components: Components::new(),
            rules,
            bounding_box: IntBox::from_coords(-5000, -5000, 5000, 5000),
            items,
            manager: SearchTreeManager::new(),
        };
        fixture.insert_all();
        fixture
    }

    pub fn ctx(&self) -> ItemCtx<'_> {
        ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        }
    }

    fn insert_all(&mut self) {
        let mut items = std::mem::take(&mut self.items);
        let ctx = ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        for item in items.values_mut() {
            self.manager.insert(item, &ctx);
        }
        self.items = items;
    }

    pub fn build_autoroute_tree(&mut self, clearance_class_index: usize) -> TreeId {
        let mut items = std::mem::take(&mut self.items);
        let ctx = ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        let mut refs: Vec<&mut Item> = items.values_mut().rev().collect();
        let id = self
            .manager
            .get_autoroute_tree(clearance_class_index, &mut refs, &ctx)
            .id();
        drop(refs);
        self.items = items;
        id
    }

    pub fn generate_keepout_outside(&mut self) {
        let mut outline = self.items.remove(&ItemId(1)).expect("the outline");
        self.manager.remove(&mut outline);
        if let Item::BoardOutline(o) = &mut outline {
            o.generate_keepout_outside(true);
        }
        let ctx = ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        self.manager.insert(&mut outline, &ctx);
        self.items.insert(ItemId(1), outline);
    }

    pub fn tree(&self, id: TreeId) -> &ShapeSearchTree {
        self.manager
            .trees()
            .find(|tree| tree.id() == id)
            .expect("the fixture only asks for trees it built")
    }
}

impl Default for AreaFixture {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TraceFixture {
    pub library: BoardLibrary,
    pub components: Components,
    pub rules: BoardRules,
    pub bounding_box: IntBox,
    pub items: BTreeMap<ItemId, Item>,
    pub manager: SearchTreeManager,
}

impl TraceFixture {
    pub fn new() -> TraceFixture {
        let rules = BoardRules::new(
            layers(),
            ClearanceMatrix::get_default_instance(&layers(), 200),
        );
        let mut items = BTreeMap::new();
        items.insert(
            ItemId(1),
            Item::BoardOutline(BoardOutline::new(
                ItemHeader::new(ItemId(1), Vec::new(), 0, 0, FixedState::SystemFixed),
                Vec::new(),
            )),
        );
        items.insert(
            ItemId(2),
            Item::Trace(trace_piece(2, &[(-500, 0), (0, 0), (0, 400)])),
        );
        items.insert(
            ItemId(3),
            Item::Trace(trace_piece(3, &[(0, 400), (500, 400), (500, 900)])),
        );
        let mut fixture = TraceFixture {
            library: BoardLibrary::new(Padstacks::new(layers()), Packages::new()),
            components: Components::new(),
            rules,
            bounding_box: BOUNDING_BOX,
            items,
            manager: SearchTreeManager::new(),
        };
        let mut items = std::mem::take(&mut fixture.items);
        let ctx = ItemCtx {
            library: &fixture.library,
            components: &fixture.components,
            rules: &fixture.rules,
            bounding_box: &fixture.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        for item in items.values_mut() {
            fixture.manager.insert(item, &ctx);
        }
        fixture.items = items;
        fixture
    }

    pub fn skewed_outline() -> TraceFixture {
        TraceFixture::outline(&[
            Point::new(0, 0),
            Point::new(3000, 500),
            Point::new(1000, 2500),
        ])
    }

    pub fn outline(corners: &[Point]) -> TraceFixture {
        let rules = BoardRules::new(
            layers(),
            ClearanceMatrix::get_default_instance(&layers(), 200),
        );
        let mut items = BTreeMap::new();
        items.insert(
            ItemId(1),
            Item::BoardOutline(BoardOutline::new(
                ItemHeader::new(ItemId(1), Vec::new(), 1, 0, FixedState::SystemFixed),
                vec![PolylineShapeRef::Polygon(
                    copper_geometry::PolygonShape::from_points(corners),
                )],
            )),
        );
        let mut fixture = TraceFixture {
            library: BoardLibrary::new(Padstacks::new(layers()), Packages::new()),
            components: Components::new(),
            rules,
            bounding_box: IntBox::from_coords(-5000, -5000, 5000, 5000),
            items,
            manager: SearchTreeManager::new(),
        };
        let mut items = std::mem::take(&mut fixture.items);
        let ctx = ItemCtx {
            library: &fixture.library,
            components: &fixture.components,
            rules: &fixture.rules,
            bounding_box: &fixture.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        for item in items.values_mut() {
            fixture.manager.insert(item, &ctx);
        }
        fixture.items = items;
        fixture
    }

    pub fn merge_pair(a: &[Point], b: &[Point]) -> TraceFixture {
        let rules = BoardRules::new(
            layers(),
            ClearanceMatrix::get_default_instance(&layers(), 200),
        );
        let mut items = BTreeMap::new();
        items.insert(
            ItemId(1),
            Item::BoardOutline(BoardOutline::new(
                ItemHeader::new(ItemId(1), Vec::new(), 0, 0, FixedState::SystemFixed),
                Vec::new(),
            )),
        );
        for (index, corners) in [a, b].into_iter().enumerate() {
            let id = ItemId(index as u32 + 2);
            items.insert(
                id,
                Item::Trace(PolylineTrace::new(
                    ItemHeader::new(id, vec![1], 1, 0, FixedState::Unfixed),
                    Polyline::from_points(corners),
                    0,
                    30,
                    None,
                )),
            );
        }
        let mut fixture = TraceFixture {
            library: BoardLibrary::new(Padstacks::new(layers()), Packages::new()),
            components: Components::new(),
            rules,
            bounding_box: BOUNDING_BOX,
            items,
            manager: SearchTreeManager::new(),
        };
        let mut items = std::mem::take(&mut fixture.items);
        let ctx = ItemCtx {
            library: &fixture.library,
            components: &fixture.components,
            rules: &fixture.rules,
            bounding_box: &fixture.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        for item in items.values_mut() {
            fixture.manager.insert(item, &ctx);
        }
        fixture.items = items;
        fixture
    }

    pub fn build_autoroute_tree(&mut self, clearance_class_index: usize) -> TreeId {
        let mut items = std::mem::take(&mut self.items);
        let ctx = ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        let mut refs: Vec<&mut Item> = items.values_mut().rev().collect();
        let id = self
            .manager
            .get_autoroute_tree(clearance_class_index, &mut refs, &ctx)
            .id();
        drop(refs);
        self.items = items;
        id
    }

    pub fn ctx(&self) -> ItemCtx<'_> {
        ItemCtx {
            library: &self.library,
            components: &self.components,
            rules: &self.rules,
            bounding_box: &self.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        }
    }

    pub fn take_trace(&mut self, id: u32) -> PolylineTrace {
        match self.items.remove(&ItemId(id)).expect("a trace") {
            Item::Trace(trace) => trace,
            _ => panic!("item {id} is not a trace"),
        }
    }

    pub fn put_trace(&mut self, id: u32, trace: PolylineTrace) {
        self.items.insert(ItemId(id), Item::Trace(trace));
    }

    pub fn tree(&self) -> &ShapeSearchTree {
        self.manager.get_default_tree()
    }
}

impl Default for TraceFixture {
    fn default() -> Self {
        Self::new()
    }
}

pub fn trace_piece(id: u32, corners: &[(i32, i32)]) -> PolylineTrace {
    PolylineTrace::new(
        ItemHeader::new(ItemId(id), vec![1], 1, 0, FixedState::Unfixed),
        Polyline::from_points(
            &corners
                .iter()
                .map(|(x, y)| Point::new(*x, *y))
                .collect::<Vec<_>>(),
        ),
        0,
        30,
        None,
    )
}

pub fn p2t11_board() -> Board {
    p2t11_board_with_host_cad(false)
}

pub fn p2t11_host_cad_board() -> Board {
    p2t11_board_with_host_cad(true)
}

fn p2t11_board_with_host_cad(host_cad: bool) -> Board {
    let ls = layers();
    let mut clearance_matrix = ClearanceMatrix::get_default_instance(&ls, 200);
    assert!(clearance_matrix.append_class("wide"));
    clearance_matrix.set_value_on_all_layers(2, 1, 600);
    clearance_matrix.set_value_on_all_layers(2, 2, 800);
    let mut rules = BoardRules::new(layers(), clearance_matrix);
    rules.create_default_net_class();
    let default_class = rules.get_default_net_class();
    if host_cad {
        rules.trace_angle_restriction = AngleRestriction::NinetyDegree;
    }

    let mut padstacks = Padstacks::new(layers());
    let smd_pad = padstacks.add(
        "smd",
        vec![
            Some(Shape::Tile(TileShape::Box(IntBox::from_coords(
                -50, -50, 50, 50,
            )))),
            None,
        ],
        false,
        false,
    );
    let thru_shape = Shape::Tile(TileShape::Box(IntBox::from_coords(-70, -70, 70, 70)));
    let thru_pad = padstacks.add(
        "thru",
        vec![Some(thru_shape.clone()), Some(thru_shape)],
        true,
        false,
    );
    let mut packages = Packages::new();
    let pkg = packages.add(
        "pkg",
        vec![
            PackagePin::new("P1", smd_pad, IntVector::new(-1000, 0).into(), 0.0),
            PackagePin::new("P2", thru_pad, IntVector::new(1000, 1000).into(), 0.0),
        ],
        None,
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        true,
    );
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(0, 0)), 0.0, true, pkg);

    let outline = vec![PolylineShapeRef::Polygon(
        copper_geometry::PolygonShape::from_points(&[
            Point::new(-5000, -5000),
            Point::new(5000, -5000),
            Point::new(5000, 5000),
            Point::new(-5000, 5000),
        ]),
    )];
    let mut board = Board::new(
        outline,
        1,
        BOUNDING_BOX,
        rules,
        BoardLibrary::new(padstacks, packages),
        components,
        if host_cad {
            Communication::new(
                Unit::Mil,
                10,
                ItemIdGenerator::new(),
                Some("KiCad".to_string()),
                Some("7.0".to_string()),
            )
        } else {
            Communication::default()
        },
    );
    board.rules.nets.add("N1", 1, false, default_class);
    board.rules.nets.add("N2", 1, false, default_class);

    board.insert_pin(1, 0, vec![1], 1, FixedState::Unfixed);
    board.insert_pin(1, 1, vec![1], 1, FixedState::Unfixed);
    board.insert_trace_without_cleaning(
        Polyline::from_points(&[Point::new(-1000, 0), Point::new(0, 0)]),
        0,
        30,
        vec![1],
        1,
        FixedState::Unfixed,
    );
    board.insert_trace_without_cleaning(
        Polyline::from_points(&[
            Point::new(0, 0),
            Point::new(1000, 0),
            Point::new(1000, 1000),
        ]),
        1,
        30,
        vec![1],
        1,
        FixedState::Unfixed,
    );
    board
        .insert_via(
            thru_pad,
            Point::new(0, 0),
            vec![1],
            1,
            FixedState::Unfixed,
            true,
        )
        .expect("no normalisation failure");
    board.insert_obstacle(
        copper_geometry::Area::Shape(Shape::Tile(TileShape::Box(IntBox::from_coords(
            2000, 2000, 3000, 3000,
        )))),
        0,
        1,
        FixedState::Unfixed,
    );
    board.insert_conduction_area(
        copper_geometry::Area::Shape(Shape::Tile(TileShape::Box(IntBox::from_coords(
            -3000, -3000, -2000, -2000,
        )))),
        0,
        vec![2],
        1,
        true,
        FixedState::Unfixed,
    );
    if host_cad {
        board.insert_obstacle(
            copper_geometry::Area::Shape(Shape::Tile(TileShape::Box(IntBox::from_coords(
                -9000, -9000, 9000, -8000,
            )))),
            0,
            1,
            FixedState::Unfixed,
        );
    }
    board
}

pub fn shove_board() -> Board {
    let ls = layers();
    let cm = ClearanceMatrix::get_default_instance(&ls, 200);
    let mut rules = BoardRules::new(layers(), cm);
    rules.create_default_net_class();
    let default_class = rules.get_default_net_class();
    let mut board = Board::new(
        Vec::new(),
        0,
        BOUNDING_BOX,
        rules,
        BoardLibrary::new(Padstacks::new(layers()), Packages::new()),
        Components::new(),
        Communication::default(),
    );
    board.rules.nets.add("N1", 1, false, default_class);
    board.rules.nets.add("N2", 1, false, default_class);
    for (corners, net) in [
        ([(-3000, 0), (3000, 0)], 1),
        ([(0, -3000), (0, 3000)], 2),
        ([(-3000, 200), (3000, 200)], 2),
    ] {
        board.insert_trace_without_cleaning(
            Polyline::from_points(&corners.map(|(x, y)| Point::new(x, y))),
            0,
            30,
            vec![net],
            1,
            FixedState::Unfixed,
        );
    }
    board
}

pub fn cycle_board() -> (Board, PadstackId) {
    let ls = layers();
    let cm = ClearanceMatrix::get_default_instance(&ls, 200);
    let mut rules = BoardRules::new(layers(), cm);
    rules.create_default_net_class();
    let default_class = rules.get_default_net_class();
    let mut padstacks = Padstacks::new(layers());
    let thru_shape = Shape::Tile(TileShape::Box(IntBox::from_coords(-70, -70, 70, 70)));
    let thru_pad = padstacks.add(
        "thru",
        vec![Some(thru_shape.clone()), Some(thru_shape)],
        true,
        false,
    );
    let mut packages = Packages::new();
    let pkg = packages.add(
        "pkg",
        Vec::new(),
        None,
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        true,
    );
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(0, 0)), 0.0, true, pkg);
    components.add_with_generated_name(Some(Point::new(0, 0)), 0.0, false, pkg);
    let mut board = Board::new(
        Vec::new(),
        0,
        BOUNDING_BOX,
        rules,
        BoardLibrary::new(padstacks, packages),
        components,
        Communication::default(),
    );
    for name in ["N1", "N2", "N3"] {
        board.rules.nets.add(name, 1, false, default_class);
    }
    for x in [0, 2000] {
        board
            .insert_via(
                thru_pad,
                Point::new(x, 0),
                vec![1],
                1,
                FixedState::Unfixed,
                true,
            )
            .expect("no normalisation failure");
    }
    board.insert_trace_without_cleaning(
        Polyline::from_points(&[Point::new(0, 0), Point::new(2000, 0)]),
        0,
        30,
        vec![1],
        1,
        FixedState::Unfixed,
    );
    board.insert_trace_without_cleaning(
        Polyline::from_points(&[
            Point::new(0, 0),
            Point::new(0, 1000),
            Point::new(2000, 1000),
            Point::new(2000, 0),
        ]),
        0,
        30,
        vec![1],
        1,
        FixedState::Unfixed,
    );
    board.insert_conduction_area(
        copper_geometry::Area::Shape(Shape::Tile(TileShape::Box(IntBox::from_coords(
            4000, 0, 6000, 2000,
        )))),
        0,
        vec![3],
        1,
        true,
        FixedState::Unfixed,
    );
    board.insert_trace_without_cleaning(
        Polyline::from_points(&[Point::new(4200, 200), Point::new(5800, 1800)]),
        0,
        30,
        vec![3],
        1,
        FixedState::Unfixed,
    );
    (board, thru_pad)
}

pub fn cycle_order_board() -> Board {
    let three_layers = LayerStructure::new(vec![
        Layer::new("front", true),
        Layer::new("inner", true),
        Layer::new("back", true),
    ]);
    let cm = ClearanceMatrix::get_default_instance(&three_layers, 200);
    let mut rules = BoardRules::new(three_layers.clone(), cm);
    rules.create_default_net_class();
    let default_class = rules.get_default_net_class();
    let mut padstacks = Padstacks::new(three_layers.clone());
    let thru_shape = Shape::Tile(TileShape::Box(IntBox::from_coords(-70, -70, 70, 70)));
    let thru_pad = padstacks.add(
        "thru",
        vec![
            Some(thru_shape.clone()),
            Some(thru_shape.clone()),
            Some(thru_shape),
        ],
        true,
        false,
    );
    let mut board = Board::new(
        Vec::new(),
        0,
        BOUNDING_BOX,
        rules,
        BoardLibrary::new(padstacks, Packages::new()),
        Components::new(),
        Communication::default(),
    );
    board.rules.nets.add("N1", 1, false, default_class);

    for x in [0, 3000] {
        board
            .insert_via(
                thru_pad,
                Point::new(x, 0),
                vec![1],
                1,
                FixedState::Unfixed,
                true,
            )
            .expect("no normalisation failure");
    }
    board.insert_trace_without_cleaning(
        Polyline::from_points(&[
            Point::new(0, 0),
            Point::new(0, -1000),
            Point::new(3000, -1000),
            Point::new(3000, 0),
        ]),
        0,
        30,
        vec![1],
        1,
        FixedState::Unfixed,
    );
    board.insert_trace_without_cleaning(
        Polyline::from_points(&[Point::new(0, 0), Point::new(0, 2000)]),
        2,
        30,
        vec![1],
        1,
        FixedState::Unfixed,
    );
    board.insert_trace_without_cleaning(
        Polyline::from_points(&[Point::new(0, 0), Point::new(3000, 0)]),
        1,
        30,
        vec![1],
        1,
        FixedState::Unfixed,
    );
    board
}

pub fn fanout_order_board() -> Board {
    let ls = layers();
    let cm = ClearanceMatrix::get_default_instance(&ls, 200);
    let mut rules = BoardRules::new(layers(), cm);
    rules.create_default_net_class();
    let default_class = rules.get_default_net_class();
    let mut padstacks = Padstacks::new(layers());
    let thru_shape = Shape::Tile(TileShape::Box(IntBox::from_coords(-70, -70, 70, 70)));
    let thru_pad = padstacks.add(
        "thru",
        vec![Some(thru_shape.clone()), Some(thru_shape)],
        true,
        false,
    );
    let mut board = Board::new(
        Vec::new(),
        0,
        BOUNDING_BOX,
        rules,
        BoardLibrary::new(padstacks, Packages::new()),
        Components::new(),
        Communication::default(),
    );
    board.rules.nets.add("N1", 1, false, default_class);

    for (x, y) in [(5000, 5000), (0, 0)] {
        board
            .insert_via(
                thru_pad,
                Point::new(x, y),
                vec![1],
                1,
                FixedState::Unfixed,
                true,
            )
            .expect("no normalisation failure");
    }
    for (corners, layer, fixed) in [
        ([(1000, 1000), (1000, 0)], 0usize, FixedState::ShoveFixed),
        ([(1000, 0), (0, 0)], 0, FixedState::Unfixed),
        ([(0, 0), (0, -1000)], 0, FixedState::Unfixed),
        ([(5000, 5000), (0, 0)], 1, FixedState::Unfixed),
        ([(5000, 5000), (1000, 1000)], 0, FixedState::Unfixed),
    ] {
        board.insert_trace_without_cleaning(
            Polyline::from_points(&corners.map(|(x, y)| Point::new(x, y))),
            layer,
            30,
            vec![1],
            1,
            fixed,
        );
    }
    board
}

pub fn descending(set: std::collections::BTreeSet<ItemId>) -> Vec<u32> {
    set.into_iter().rev().map(|id| id.0).collect()
}

pub fn nums(ids: impl IntoIterator<Item = ItemId>) -> Vec<u32> {
    ids.into_iter().map(|id| id.0).collect()
}
