"""mongod checks a stage spec field-by-field, so ORDER decides the message.

Pinned against mongod 8.2.11 (2026-09-01). An unknown or specifically-missing
field is reported before the generic "requires X and Y", so `{$bucket: {a: 1}}`
names `a` rather than listing what is absent. Each stage has its own code and
wording for that, and three stages invert the rule -- `$lookup` and
`$graphLookup` report a missing `from` first, and `$geoNear` reports a missing
`near` before it objects to the spec's own type.

`tools/probes/aggregation_stage_specs.py` is the standing cover: 725 shapes,
0 divergent.
"""

import pytest

from secantus.aggregate import AggregateError, apply_pipeline
from secantus.storage import Storage


@pytest.fixture
def ctx(tmp_path):
    from secantus.aggregate import PipelineContext

    storage = Storage(str(tmp_path))
    try:
        storage.insert("db", "c", [{"_id": 1}])
        yield PipelineContext(storage=storage, db_name="db", coll_name="c")
    finally:
        storage.close()


def fails(ctx, stage):
    with pytest.raises(AggregateError) as exc:
        apply_pipeline([{"_id": 1}], [stage], ctx)
    return exc.value.code, str(exc.value)


class TestUnknownFieldBeatsTheGenericMessage:
    @pytest.mark.parametrize(
        ("stage", "code", "message"),
        [
            ({"$replaceRoot": {"a": 1}}, 40415, "BSON field '$replaceRoot.a' is an unknown field."),
            ({"$sample": {"a": 1}}, 28748, "unrecognized option to $sample: a"),
            ({"$bucket": {"a": 1}}, 40197, "Unrecognized option to $bucket: a."),
            ({"$bucketAuto": {"a": 1}}, 40245, "Unrecognized option to $bucketAuto: a"),
            ({"$densify": {"a": 1}}, 40415, "BSON field '$densify.a' is an unknown field."),
            ({"$fill": {"a": 1}}, 40415, "BSON field '$fill.a' is an unknown field."),
            ({"$unionWith": {"a": 1}}, 40415, "BSON field '$unionWith.a' is an unknown field."),
        ],
    )
    def test_named_rather_than_listed(self, ctx, stage, code, message):
        assert fails(ctx, stage) == (code, message)

    def test_it_wins_even_when_a_required_field_is_also_missing(self, ctx):
        """`$bucket` is missing both `groupBy` and `boundaries` here."""
        assert fails(ctx, {"$bucket": {"a": 1}})[0] == 40197


class TestMissingRequiredField:
    @pytest.mark.parametrize(
        ("stage", "code", "message"),
        [
            (
                {"$replaceRoot": {}},
                40414,
                "BSON field '$replaceRoot.newRoot' is missing but a required field",
            ),
            (
                {"$densify": {}},
                40414,
                "BSON field '$densify.field' is missing but a required field",
            ),
            ({"$fill": {}}, 40414, "BSON field '$fill.output' is missing but a required field"),
            ({"$sample": {}}, 28749, "$sample stage must specify a size"),
        ],
    )
    def test_reported_after_the_unknown_pass(self, ctx, stage, code, message):
        assert fails(ctx, stage) == (code, message)


class TestStagesThatInvertTheRule:
    def test_lookup_reports_a_missing_from_first(self, ctx):
        code, message = fails(ctx, {"$lookup": {"a": 1}})
        assert (code, message) == (9, "must specify 'pipeline' when 'from' is empty")

    def test_but_an_unknown_field_wins_once_from_is_present(self, ctx):
        code, _ = fails(ctx, {"$lookup": {"from": "c", "zz": 1}})
        assert code == 40415

    @pytest.mark.parametrize(
        ("spec", "rendered"),
        [({}, "{}"), ({"a": 1}, "{ a: 1 }"), ({"a": 1, "b": "x"}, '{ a: 1, b: "x" }')],
    )
    def test_graph_lookup_echoes_the_whole_spec(self, ctx, spec, rendered):
        """And in mongod's SPACED document rendering, unlike the value one."""
        code, message = fails(ctx, {"$graphLookup": spec})
        assert code == 9
        assert message == (f"missing 'from' option to $graphLookup stage specification: {rendered}")

    @pytest.mark.parametrize("spec", [{}, {"a": 1}, [], [1]])
    def test_geo_near_reports_a_missing_near_before_the_spec_type(self, ctx, spec):
        assert fails(ctx, {"$geoNear": spec}) == (5860400, "$geoNear requires a 'near' argument")

    @pytest.mark.parametrize("spec", ["x", 5, 1.5, True, None])
    def test_but_a_SCALAR_spec_is_still_a_type_error(self, ctx, spec):
        """An array is a document in BSON, so it gets the `near` message; a
        scalar cannot be a spec at all."""
        assert fails(ctx, {"$geoNear": spec})[0] == 10065


class TestGroup:
    def test_a_non_accumulator_field_beats_the_missing_id(self, ctx):
        assert fails(ctx, {"$group": {"a": 1}}) == (
            40234,
            "The field 'a' must be an accumulator object",
        )

    def test_the_missing_id_still_reports_when_the_fields_are_accumulators(self, ctx):
        assert fails(ctx, {"$group": {"x": {"$sum": 1}}})[0] == 15955


class TestUnionWith:
    def test_an_empty_collection_name_is_not_a_namespace(self, ctx):
        """This used to return the outer documents unchanged -- a wrong ANSWER,
        not a message difference."""
        assert fails(ctx, {"$unionWith": ""}) == (
            73,
            "Namespace dbis not a valid collection name",
        )

    def test_a_spec_with_no_coll_asks_for_a_documents_pipeline(self, ctx):
        assert fails(ctx, {"$unionWith": {}}) == (
            9,
            "$unionWith stage without explicit collection must have a pipeline "
            "with $documents as first stage",
        )

    def test_a_non_string_coll_names_its_type(self, ctx):
        code, message = fails(ctx, {"$unionWith": {"coll": 5}})
        assert code == 14
        assert message == (
            "BSON field '$unionWith.coll' is the wrong type 'int', expected type 'string'"
        )


class TestDocuments:
    def test_an_empty_document_spec_is_rejected_before_the_namespace_check(self, ctx):
        assert fails(ctx, {"$documents": {}}) == (
            51270,
            "Invalid empty sub-projection: _tempDocumentsField",
        )

    @pytest.mark.parametrize("spec", [{"a": 1}, [], [{"x": 1}]])
    def test_everything_else_still_gets_the_namespace_error(self, ctx, spec):
        assert fails(ctx, {"$documents": spec})[0] == 73
