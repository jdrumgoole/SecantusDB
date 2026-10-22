--
-- PostgreSQL database dump
--



SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

--
-- Name: mood; Type: TYPE; Schema: public; Owner: postgres
--

CREATE TYPE public.mood AS ENUM (
    'sad',
    'ok',
    'happy'
);


ALTER TYPE public.mood OWNER TO postgres;

--
-- Name: pair; Type: TYPE; Schema: public; Owner: postgres
--

CREATE TYPE public.pair AS (
	a integer,
	b text
);


ALTER TYPE public.pair OWNER TO postgres;

--
-- Name: posint; Type: DOMAIN; Schema: public; Owner: postgres
--

CREATE DOMAIN public.posint AS integer
	CONSTRAINT posint_check CHECK ((VALUE > 0));


ALTER DOMAIN public.posint OWNER TO postgres;

--
-- Name: add1(integer); Type: FUNCTION; Schema: public; Owner: postgres
--

CREATE FUNCTION public.add1(x integer) RETURNS integer
    LANGUAGE sql IMMUTABLE
    AS $$ select x + 1 $$;


ALTER FUNCTION public.add1(x integer) OWNER TO postgres;

--
-- Name: FUNCTION add1(x integer); Type: COMMENT; Schema: public; Owner: postgres
--

COMMENT ON FUNCTION public.add1(x integer) IS 'adds one';


--
-- Name: trg(); Type: FUNCTION; Schema: public; Owner: postgres
--

CREATE FUNCTION public.trg() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ begin new.name := upper(new.name); return new; end $$;


ALTER FUNCTION public.trg() OWNER TO postgres;

SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: base; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.base (
    x integer
);


ALTER TABLE public.base OWNER TO postgres;

--
-- Name: c; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.c (
    id bigint NOT NULL,
    pid integer,
    amount numeric(10,2) DEFAULT 0 NOT NULL,
    tags text[],
    meta jsonb,
    CONSTRAINT c_amount_check CHECK ((amount >= (0)::numeric))
);


ALTER TABLE public.c OWNER TO postgres;

--
-- Name: c_id_seq; Type: SEQUENCE; Schema: public; Owner: postgres
--

CREATE SEQUENCE public.c_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


ALTER TABLE public.c_id_seq OWNER TO postgres;

--
-- Name: c_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: postgres
--

ALTER SEQUENCE public.c_id_seq OWNED BY public.c.id;


--
-- Name: child; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.child (
    y integer
)
INHERITS (public.base);


ALTER TABLE public.child OWNER TO postgres;

--
-- Name: ident; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.ident (
    id integer NOT NULL,
    g integer GENERATED ALWAYS AS ((id * 2)) STORED,
    note text
);


ALTER TABLE public.ident OWNER TO postgres;

--
-- Name: ident_id_seq; Type: SEQUENCE; Schema: public; Owner: postgres
--

ALTER TABLE public.ident ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME public.ident_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);


--
-- Name: p; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.p (
    id integer NOT NULL,
    name character varying(20) NOT NULL,
    m public.mood DEFAULT 'ok'::public.mood,
    d public.posint,
    created timestamp with time zone DEFAULT now()
);


ALTER TABLE public.p OWNER TO postgres;

--
-- Name: COLUMN p.name; Type: COMMENT; Schema: public; Owner: postgres
--

COMMENT ON COLUMN public.p.name IS 'the name';


--
-- Name: parted; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.parted (
    k integer,
    v text
)
PARTITION BY RANGE (k);


ALTER TABLE public.parted OWNER TO postgres;

--
-- Name: parted_1; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.parted_1 (
    k integer,
    v text
);


ALTER TABLE public.parted_1 OWNER TO postgres;

--
-- Name: pmv; Type: MATERIALIZED VIEW; Schema: public; Owner: postgres
--

CREATE MATERIALIZED VIEW public.pmv AS
 SELECT c.pid,
    count(*) AS n
   FROM public.c
  GROUP BY c.pid
  WITH NO DATA;


ALTER TABLE public.pmv OWNER TO postgres;

--
-- Name: pv; Type: VIEW; Schema: public; Owner: postgres
--

CREATE VIEW public.pv AS
 SELECT p.id,
    p.name
   FROM public.p
  WHERE (p.id > 0);


ALTER TABLE public.pv OWNER TO postgres;

--
-- Name: seq1; Type: SEQUENCE; Schema: public; Owner: postgres
--

CREATE SEQUENCE public.seq1
    START WITH 5
    INCREMENT BY 2
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


ALTER TABLE public.seq1 OWNER TO postgres;

--
-- Name: parted_1; Type: TABLE ATTACH; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.parted ATTACH PARTITION public.parted_1 FOR VALUES FROM (0) TO (100);


--
-- Name: c id; Type: DEFAULT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.c ALTER COLUMN id SET DEFAULT nextval('public.c_id_seq'::regclass);


--
-- Data for Name: base; Type: TABLE DATA; Schema: public; Owner: postgres
--

COPY public.base (x) FROM stdin;
\.


--
-- Data for Name: c; Type: TABLE DATA; Schema: public; Owner: postgres
--

COPY public.c (id, pid, amount, tags, meta) FROM stdin;
1	1	2.50	{a,b}	{"k": 1}
\.


--
-- Data for Name: child; Type: TABLE DATA; Schema: public; Owner: postgres
--

COPY public.child (x, y) FROM stdin;
\.


--
-- Data for Name: ident; Type: TABLE DATA; Schema: public; Owner: postgres
--

COPY public.ident (id, note) FROM stdin;
1	n1
\.


--
-- Data for Name: p; Type: TABLE DATA; Schema: public; Owner: postgres
--

COPY public.p (id, name, m, d, created) FROM stdin;
1	ONE	happy	3	2020-01-01 00:00:00+00
\.


--
-- Data for Name: parted_1; Type: TABLE DATA; Schema: public; Owner: postgres
--

COPY public.parted_1 (k, v) FROM stdin;
\.


--
-- Name: c_id_seq; Type: SEQUENCE SET; Schema: public; Owner: postgres
--

SELECT pg_catalog.setval('public.c_id_seq', 1, true);


--
-- Name: ident_id_seq; Type: SEQUENCE SET; Schema: public; Owner: postgres
--

SELECT pg_catalog.setval('public.ident_id_seq', 1, true);


--
-- Name: seq1; Type: SEQUENCE SET; Schema: public; Owner: postgres
--

SELECT pg_catalog.setval('public.seq1', 5, false);


--
-- Name: c c_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.c
    ADD CONSTRAINT c_pkey PRIMARY KEY (id);


--
-- Name: p p_name_key; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.p
    ADD CONSTRAINT p_name_key UNIQUE (name);


--
-- Name: p p_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.p
    ADD CONSTRAINT p_pkey PRIMARY KEY (id);


--
-- Name: c_pid_idx; Type: INDEX; Schema: public; Owner: postgres
--

CREATE INDEX c_pid_idx ON public.c USING btree (pid);


--
-- Name: p_lower_name; Type: INDEX; Schema: public; Owner: postgres
--

CREATE UNIQUE INDEX p_lower_name ON public.p USING btree (lower((name)::text));


--
-- Name: p p_upper; Type: TRIGGER; Schema: public; Owner: postgres
--

CREATE TRIGGER p_upper BEFORE INSERT ON public.p FOR EACH ROW EXECUTE FUNCTION public.trg();


--
-- Name: c c_pid_fkey; Type: FK CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.c
    ADD CONSTRAINT c_pid_fkey FOREIGN KEY (pid) REFERENCES public.p(id) ON DELETE CASCADE;


--
-- Name: c; Type: ROW SECURITY; Schema: public; Owner: postgres
--

ALTER TABLE public.c ENABLE ROW LEVEL SECURITY;

--
-- Name: c cpol; Type: POLICY; Schema: public; Owner: postgres
--

CREATE POLICY cpol ON public.c FOR SELECT USING ((amount > (0)::numeric));


--
-- Name: TABLE p; Type: ACL; Schema: public; Owner: postgres
--

GRANT SELECT ON TABLE public.p TO rp26_reader;


--
-- Name: pmv; Type: MATERIALIZED VIEW DATA; Schema: public; Owner: postgres
--

REFRESH MATERIALIZED VIEW public.pmv;


--
-- PostgreSQL database dump complete
--


