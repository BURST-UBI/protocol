`electorate-257.tar.gz` contains a synthetic Dev ledger and its saved parameters.
Three accounts are enrolled; ordinary certified selection and activation have
prepared the remaining 254 applicants. All signing keys come from the public
test seeds. No production accounts or private credentials are included.

The slow CI tests authenticate a copy, then exercise signed enrollment, exact
membership and snapshot recovery. Reusing this prerequisite avoids repeating
its expensive preparation; it does not prove fresh network onboarding.

To regenerate it on a capped validation host, run the ignored
`signed_incremental_electorate_resource_probe` with `BURST_ELECTORATE_HUMANS=257`,
`BURST_ELECTORATE_BATCH=128`, `BURST_ELECTORATE_UNTIL=3` and an empty
`BURST_ELECTORATE_PREPARED` directory. Retain interrupted preparation for the
existing `BURST_ELECTORATE_PREPARE_RESUME` path. Archive only `data.mdb` and
`resource-probe.bin` beneath the generated parameter-hash directory, then update
the checksum. Changed fixture parameters require a new matching preparation.
