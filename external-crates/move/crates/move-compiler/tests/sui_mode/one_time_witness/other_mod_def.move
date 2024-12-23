// invalid, one-time witness type candidate used in a different module

module a::n {
    use sui::hc;
    use sui::tx_context;

    fun init(_otw: hc::HC, _ctx: &mut tx_context::TxContext) {
    }

}


module sui::tx_context {
    struct TxContext has drop {}
}

module sui::hc {
    struct HC has drop {}
}