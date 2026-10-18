use stationdeck_ui::DeckModule;

pub fn modules() -> Vec<Box<dyn DeckModule>> {
    vec![
        Box::<stationdeck_demo::DemoModule>::default(),
        Box::<stationdeck_grpc::GrpcModule>::default(),
        Box::<stationdeck_config_ui::ConfigModule>::default(),
    ]
}
