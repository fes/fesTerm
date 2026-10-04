// This executable tests production quad preparation without creating a graphics device.
#include "renderer.cpp"
#include <cstdlib>
#include <iostream>
#include <new>

namespace {
struct Allocations {
    size_t calls{}, bytes{};
};
thread_local Allocations* counted_allocations = nullptr;
}

void* operator new(size_t bytes) {
    if (counted_allocations) {
        ++counted_allocations->calls;
        counted_allocations->bytes += bytes;
    }
    if (auto* pointer = std::malloc(bytes ? bytes : 1)) return pointer;
    throw std::bad_alloc();
}

void operator delete(void* pointer) noexcept { std::free(pointer); }
void operator delete(void* pointer, size_t) noexcept { std::free(pointer); }

namespace festerm_direct2d {

static_assert(sizeof(std::array<const Vertex*, 2>) + sizeof(size_t) == 24);

static void require(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}

// Frozen pre-change predicate: the allocation control must include actual quad
// validation and Draw construction, not an isolated vector microbenchmark.
static bool reference_quad(const std::array<Vertex, 6>& vertices, Draw& result, const Texture& texture) {
    auto left = vertices[0].x, right = left, top = vertices[0].y, bottom = top;
    for (const auto& v : vertices) {
        left = std::min(left, v.x); right = std::max(right, v.x);
        top = std::min(top, v.y); bottom = std::max(bottom, v.y);
        if (!(v.color == vertices[0].color)) return false;
    }
    if (right <= left || bottom <= top) return false;
    std::array<const Vertex*, 4> corners{};
    for (const auto& v : vertices) {
        if ((v.x != left && v.x != right) || (v.y != top && v.y != bottom)) return false;
        const int index = (v.x == right ? 1 : 0) + (v.y == bottom ? 2 : 0);
        if (corners[index] && (corners[index]->u != v.u || corners[index]->v != v.v)) return false;
        corners[index] = &v;
    }
    for (auto corner : corners) if (!corner) return false;
    std::vector<Vertex> shared;
    for (size_t i = 0; i < 3; ++i)
        for (size_t j = 3; j < 6; ++j)
            if (vertices[i].x == vertices[j].x && vertices[i].y == vertices[j].y)
                shared.push_back(vertices[i]);
    if (shared.size() != 2 || shared[0].x == shared[1].x || shared[0].y == shared[1].y)
        return false;
    const auto area = [](Vertex a, Vertex b, Vertex c) {
        return std::abs((b.x-a.x)*(c.y-a.y)-(c.x-a.x)*(b.y-a.y))/2;
    };
    if (std::abs(area(vertices[0],vertices[1],vertices[2]) +
        area(vertices[3],vertices[4],vertices[5]) - (right-left)*(bottom-top)) > 0.01f)
        return false;
    result.destination = {left, top, right, bottom};
    result.color = vertices[0].color;
    if (std::all_of(vertices.begin(), vertices.end(), [](auto v) { return v.u == 0 && v.v == 0; })) {
        result.kind = Draw::Rectangle;
        return true;
    }
    if (corners[0]->u != corners[2]->u || corners[1]->u != corners[3]->u ||
        corners[0]->v != corners[1]->v || corners[2]->v != corners[3]->v ||
        corners[1]->u <= corners[0]->u || corners[2]->v <= corners[0]->v)
        return false;
    result.source = {corners[0]->u*texture.width, corners[0]->v*texture.height,
        corners[3]->u*texture.width, corners[3]->v*texture.height};
    result.kind = texture.mask ? Draw::Mask : Draw::Bitmap;
    result.bitmap = texture.bitmap;
    if (!texture.mask && (result.color.r != result.color.a ||
        result.color.g != result.color.a || result.color.b != result.color.a))
        throw UnsupportedFrame("Unsupported tinted color bitmap");
    return true;
}

struct Prepared {
    Draw draw{};
    bool accepted{}, unsupported{};
    Allocations allocations{};
};

using Quad = bool (*)(const std::array<Vertex, 6>&, Draw&, const Texture&);

static Prepared prepare(Quad function, const std::array<Vertex, 6>& vertices, const Texture& texture) {
    Prepared result;
    counted_allocations = &result.allocations;
    try {
        result.accepted = function(vertices, result.draw, texture);
    } catch (const UnsupportedFrame&) {
        result.unsupported = true;
    } catch (...) {
        counted_allocations = nullptr;
        throw;
    }
    counted_allocations = nullptr;
    return result;
}

static bool equal_rect(D2D1_RECT_F left, D2D1_RECT_F right) {
    return left.left == right.left && left.top == right.top &&
        left.right == right.right && left.bottom == right.bottom;
}

static void equal_preparation(const Prepared& before, const Prepared& after) {
    require(before.accepted == after.accepted && before.unsupported == after.unsupported,
        "Quad acceptance or typed refusal changed");
    const auto& left = before.draw;
    const auto& right = after.draw;
    require(left.kind == right.kind && equal_rect(left.destination, right.destination) &&
        equal_rect(left.source, right.source) && left.color == right.color &&
        left.bitmap.Get() == right.bitmap.Get() &&
        left.gradient.Get() == right.gradient.Get() && left.fill.Get() == right.fill.Get() &&
        left.transform._11 == right.transform._11 && left.transform._12 == right.transform._12 &&
        left.transform._21 == right.transform._21 && left.transform._22 == right.transform._22 &&
        left.transform._31 == right.transform._31 && left.transform._32 == right.transform._32,
        "Prepared geometry, UV, color, alpha, transform or resource changed");
}

static std::array<Vertex, 6> glyph(Color color, float scale = 1) {
    const Vertex a{1.25f*scale, 2.5f*scale, 0.125f, 0.25f, color};
    const Vertex b{9.75f*scale, 2.5f*scale, 0.5f, 0.25f, color};
    const Vertex c{9.75f*scale, 20.5f*scale, 0.5f, 0.75f, color};
    const Vertex d{1.25f*scale, 20.5f*scale, 0.125f, 0.75f, color};
    return {a,b,c,a,c,d};
}

static void quad_scratch_cold_warm_mutation_and_refusal() {
    const Color white{255,255,255,255}, translucent{80,40,20,128};
    Texture texture{64,128,true,{}};
    size_t calls = 0;
    for (float scale : {1.0f, 1.25f, 1.5f, 2.0f}) {
        for (size_t state = 0; state < 8; ++state) {
            auto vertices = glyph(white, scale);
            texture.width = state == 2 ? 128 : 64;
            texture.height = state == 2 ? 64 : 128;
            texture.mask = state != 3 && state != 5;
            if (state == 4 || state == 5)
                for (auto& vertex : vertices) vertex.color = translucent;
            if (state == 6)
                for (auto& vertex : vertices) vertex.u = vertex.v = 0;
            const auto before = prepare(reference_quad, vertices, texture);
            const auto after = prepare(quad, vertices, texture);
            equal_preparation(before, after);
            require(before.allocations.calls == after.allocations.calls + 2 &&
                before.allocations.bytes == after.allocations.bytes + 3*sizeof(Vertex),
                "Expected two eliminated shared-corner allocations");
            if (state == 5) {
                require(after.unsupported && !after.accepted, "Tinted bitmap did not refuse");
            } else {
                require(after.accepted && !after.unsupported && after.allocations.calls == 0,
                    "Supported quad allocated scratch or failed after mutation/refusal");
            }
            ++calls;
        }
    }
    require(calls == 32, "Cold/warm/mutation/refusal cases were skipped");
    std::cout << "32 cold/warm/mutation/refusal preparations: 64 fewer allocations, "
        << 32*3*sizeof(Vertex) << " fewer allocated bytes\n";
}

static void quad_scratch_matches_exhaustive_topologies() {
    const auto valid = glyph({255,255,255,255});
    const std::array<Vertex, 4> corners{valid[0],valid[1],valid[2],valid[5]};
    const Texture texture{64,128,true,{}};
    size_t accepted = 0;
    for (size_t topology = 0; topology < 4096; ++topology) {
        auto code = topology;
        std::array<Vertex, 6> vertices{};
        for (auto& vertex : vertices) {
            vertex = corners[code % 4];
            code /= 4;
        }
        const auto before = prepare(reference_quad, vertices, texture);
        const auto after = prepare(quad, vertices, texture);
        equal_preparation(before, after);
        require(after.allocations.calls == 0, "Quad topology allocated scratch");
        accepted += after.accepted;
    }
    require(accepted == 144, "Expected all diagonal/winding/order combinations");
    for (size_t mutation = 0; mutation < 7; ++mutation) {
        auto vertices = valid;
        if (mutation == 0) vertices[3].u += 0.125f;
        if (mutation == 1) vertices[1].color = {128,128,128,128};
        if (mutation == 2) vertices[1].x -= 0.25f;
        if (mutation == 3) for (auto& vertex : vertices) vertex.x = 1;
        if (mutation == 4) for (auto& vertex : vertices) vertex.u = 0.5f-vertex.u;
        if (mutation == 5) vertices[4] = vertices[1];
        if (mutation == 6) for (auto& vertex : vertices) vertex.v = 0.75f-vertex.v;
        const auto before = prepare(reference_quad, vertices, texture);
        const auto after = prepare(quad, vertices, texture);
        equal_preparation(before, after);
        require(!after.accepted && !after.unsupported && after.allocations.calls == 0,
            "Malformed quad changed rejection or allocated scratch");
    }
    std::cout << "4096 topologies and 7 malformed cases: identical prepared operations/refusals\n";
}

static void quad_scratch_dense_preparation_work_counts() {
    const Texture texture{512,512,true,{}};
    size_t calls = 0, before_allocations = 0, after_allocations = 0, before_bytes = 0;
    for (size_t frame = 0; frame < 3; ++frame) {
        for (size_t cell = 0; cell < 120*40; ++cell) {
            auto vertices = glyph(frame == 2 ? Color{64,128,192,255} : Color{255,255,255,255});
            for (auto& vertex : vertices) {
                vertex.x += float(cell % 120)*12;
                vertex.y += float(cell / 120)*24;
            }
            const auto before = prepare(reference_quad, vertices, texture);
            const auto after = prepare(quad, vertices, texture);
            equal_preparation(before, after);
            require(after.accepted && !after.unsupported, "Dense glyph preparation was skipped");
            ++calls;
            before_allocations += before.allocations.calls;
            after_allocations += after.allocations.calls;
            before_bytes += before.allocations.bytes;
        }
    }
    require(calls == 14400 && before_allocations == calls*2 && after_allocations == 0 &&
        before_bytes == calls*3*sizeof(Vertex), "Dense preparation allocation/work counts changed");
    std::cout << calls << " cold/warm/recolored glyph preparations: " << before_allocations
        << " -> " << after_allocations << " scratch allocations; " << before_bytes
        << " -> 0 scratch allocation bytes; same glyph count\n";
}

static void quad_scratch_preserves_boundary_failure_types() {
    Bridge bridge;
    const Texture bitmap{64,128,false,{}};
    Draw draw{};
    const auto tinted = glyph({80,40,20,128});
    require(boundary(&bridge, [&] { quad(tinted, draw, bitmap); }) == E_NOTIMPL &&
        festerm_d2d_error_is_unsupported_frame(&bridge), "Tinted quad lost typed capability refusal");
    require(boundary(&bridge, [] { throw GraphicsFailure(E_NOTIMPL); }) == E_NOTIMPL &&
        !festerm_d2d_error_is_unsupported_frame(&bridge), "Device error became a capability refusal");
    require(boundary(&bridge, [] { throw std::bad_alloc(); }) == E_OUTOFMEMORY &&
        !festerm_d2d_error_is_unsupported_frame(&bridge), "Allocation failure became a capability refusal");
    require(boundary(&bridge, [&] {
        require(quad(glyph({255,255,255,255}), draw, bitmap), "Supported quad failed after refusal");
    }) == S_OK && !festerm_d2d_error_is_unsupported_frame(&bridge),
        "Successful preparation retained stale failure classification");
    std::cout << "4 typed boundary refusal/failure/recovery checks passed\n";
}

} // namespace festerm_direct2d

int main() {
    using namespace festerm_direct2d;
    try {
        quad_scratch_cold_warm_mutation_and_refusal();
        quad_scratch_matches_exhaustive_topologies();
        quad_scratch_dense_preparation_work_counts();
        quad_scratch_preserves_boundary_failure_types();
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
