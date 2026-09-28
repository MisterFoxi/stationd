//! Clients gRPC de stationd (types + stubs générés), sans aucune dépendance au
//! daemon. Mêmes noms de modules que `stationd::proto`.

pub mod station {
    tonic::include_proto!("station");
}

pub mod schedule {
    tonic::include_proto!("webradio.schedule.v1");
}

pub mod library {
    tonic::include_proto!("webradio.library.v1");
}

pub mod plugin {
    tonic::include_proto!("webradio.plugin.v1");
}

pub mod broadcast {
    tonic::include_proto!("webradio.broadcast.v1");
}

pub mod liquidsoap {
    tonic::include_proto!("webradio.liquidsoap.v1");
}

pub mod icecast {
    tonic::include_proto!("webradio.icecast.v1");
}

pub mod live {
    tonic::include_proto!("webradio.live.v1");
}

pub mod stats {
    tonic::include_proto!("webradio.stats.v1");
}

pub mod onair {
    tonic::include_proto!("webradio.onair.v1");
}

pub mod playlist {
    tonic::include_proto!("webradio.playlist.v1");
}
