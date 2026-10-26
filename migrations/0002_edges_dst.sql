-- D-114: neighbors() walks edges in both directions; the edges primary key leads on src.
create index on memory.edges (dst);
